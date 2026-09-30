//! The organize DAT matching pass: every Keep plan is matched against the
//! Playmatch database, producing a [`GameRef`] for hash-verified hits and a
//! rename candidate per unit for the later naming pass.

use super::plan::{Decision, GameRef, UnitPlan};
use crate::dat::PlaymatchClient;
use crate::dat::RomDigests;
use crate::dat::client::{BULK_MAX_ITEMS, MAX_IN_FLIGHT};
use crate::dat::model::{
    BulkIdentifyItem, BulkIdentifyRelationsResult, BulkItemStatus, GameAndRelationMatchResult,
    GameFileMatchSearch,
};
use crate::dat::rename::{RenameAction, RenameCandidate, plan_renames};
use crate::dat::run::{rename_candidate, resolve_unit};
use crate::dat::tags::GameTags;
use crate::dat::units::{ARCHIVE_IMAGE_EXTS, DatUnit, digest_unit, is_tierable, search_name};
use crate::dat::verdict::{DatVerdict, MatchStrength, match_strength};
use crate::runner::is_cancelled_error;
use crate::util::hash::{MultiHasher, pump_skip_pad};
use crate::util::{
    CancelToken, Cancelled, FileDigests, HashAlgo, HashCache, NoProgress, ProgressReporter,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The DAT naming inputs for one unit: its rename candidate and the match's
/// platform label. Applied to the surviving plans by [`apply_dat_naming`]
/// once selection has run.
#[derive(Clone)]
pub(super) struct DatNaming {
    pub candidate: RenameCandidate,
    pub platform: Option<String>,
}

impl Default for DatNaming {
    fn default() -> Self {
        Self {
            candidate: RenameCandidate {
                path: PathBuf::new(),
                game_id: None,
                game_name: None,
                file_name: None,
                verified: false,
            },
            platform: None,
        }
    }
}

/// Plan-time DAT digests for an archive member. Retry failures are held
/// separately so the primary digest can still be matched.
pub(super) struct StagedDigests {
    pub digests: anyhow::Result<RomDigests>,
    pub retry: Option<anyhow::Result<Vec<RetryVariant>>>,
}

/// One page unit's first-pass state: the API target and search name, the
/// first identify digests, optional plain-file escalation digests, and any
/// precomputed headerless/padded retries.
struct FirstPass {
    target: DatUnit,
    name: String,
    digests: anyhow::Result<RomDigests>,
    full: Option<Vec<HashAlgo>>,
    retry: Option<anyhow::Result<Vec<RetryVariant>>>,
}

/// Matches every Keep, unpatched plan against the Playmatch database,
/// producing one [`DatNaming`] per unit (indexed by `plan.index`, default
/// for unmatched units). Archive members use their plan-time digests and
/// output basis; the bounded staging retention is only for execution.
/// Staged digests are uncached, while plain units match through the hash
/// cache. The first-pass identify is batched: every page of
/// `BULK_MAX_ITEMS * MAX_IN_FLIGHT` units goes through one bulk relations
/// request. What a bulk item cannot express stays per unit: plain-file
/// checksum escalation, cue-set track reconciliation, and headerless/padded
/// retries. Hash-verified matches build the plan's [`GameRef`]; network and
/// digest failures degrade the unit to keep-name, record the failure in
/// `match_error` (clean suspends for the run), and warn; only cancellation
/// propagates.
pub(super) async fn match_plans(
    req: &crate::runner::models::RunRequest,
    units: &[DatUnit],
    plans: &mut [UnitPlan],
    bounds: &crate::util::ChecksumBounds,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<Vec<DatNaming>> {
    let algos = [HashAlgo::Crc32, HashAlgo::Sha1];
    let client = PlaymatchClient::new(req.options.api_base.as_deref());
    let mut naming = vec![DatNaming::default(); units.len()];
    // Skip decisions never dispatch. (Patched variants cannot appear here:
    // matching runs before patch expansion.)
    let keep: Vec<usize> = plans
        .iter()
        .filter(|plan| !matches!(plan.decision, Decision::Skip(_)))
        .map(|plan| plan.index)
        .collect();
    // Every Keep plan starts at keep-name with no match, so a degraded
    // lookup still leaves the unit's real path in `naming` and plan_renames
    // keeps seeing it for the disc-set and collision guards.
    for &index in &keep {
        let primary = units[index].display_path().to_path_buf();
        naming[index] = DatNaming {
            candidate: rename_candidate(primary, None),
            platform: None,
        };
    }

    for page in keep.chunks(BULK_MAX_ITEMS * MAX_IN_FLIGHT) {
        let mut firsts: Vec<FirstPass> = Vec::with_capacity(page.len());
        for &index in page {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let unit = &units[index];
            if let Some(staged) = plans[index]
                .member
                .as_mut()
                .and_then(|member| member.digests.take())
            {
                let basis = plans[index]
                    .member
                    .as_ref()
                    .expect("staged digests have member facts")
                    .basis
                    .clone();
                let target = DatUnit::File(basis);
                let name = search_name(&target, None);
                firsts.push(FirstPass {
                    target,
                    name,
                    digests: staged.digests,
                    full: None,
                    retry: staged.retry,
                });
                continue;
            }
            let target = unit.clone();
            let (floor, escalation) = if is_tierable(&target) {
                bounds.split(&algos)
            } else {
                (algos.to_vec(), Vec::new())
            };
            let full = (!escalation.is_empty()).then(|| {
                floor
                    .iter()
                    .chain(escalation.iter())
                    .copied()
                    .collect::<Vec<HashAlgo>>()
            });
            let digests = match digest_unit(
                &target,
                &floor,
                req.ctx.hash_cache.as_deref(),
                progress,
                cancel,
            )
            .await
            {
                Ok(digests) => Ok(digests),
                Err(err) => Err(anyhow::Error::from(err)),
            };
            let name = search_name(&target, None);
            firsts.push(FirstPass {
                target,
                name,
                digests,
                full,
                retry: None,
            });
        }

        // The first-pass identify, batched for the page's single-stream
        // digests; cue sets resolve per unit below.
        let items = bulk_items(&firsts);
        let mut bulk = None;
        let mut page_error = None;
        if !items.is_empty() {
            match client.identify_bulk_relations(items, cancel).await {
                Ok(results) => bulk = Some(results),
                Err(err) => {
                    let err = anyhow::Error::from(err);
                    if is_cancelled_error(&err) {
                        return Err(err);
                    }
                    // A failed page request fails its singles: each degrades
                    // with the same warning a per-unit lookup would produce.
                    page_error = Some(err);
                }
            }
        }
        let mut retries: Vec<_> = firsts.iter_mut().map(|first| first.retry.take()).collect();
        let outcomes = bulk_outcomes(&firsts, bulk, page_error.as_ref());

        for (pos, outcome) in outcomes.into_iter().enumerate() {
            let index = page[pos];
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let primary = units[index].display_path().to_path_buf();
            let first = &firsts[pos];
            let cache = req.ctx.hash_cache.as_deref();
            let (mut matched, mut verdict, mut platform) = match outcome {
                BulkOutcome::DigestError(err) => {
                    if is_cancelled_error(err) || cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    degrade(&mut plans[index], err, &primary, progress);
                    continue;
                }
                BulkOutcome::BulkError(detail) => {
                    degrade(&mut plans[index], detail, &primary, progress);
                    continue;
                }
                BulkOutcome::Bulk {
                    matched,
                    verdict,
                    platform,
                } => (
                    matched.map(|matched| *matched),
                    std::borrow::Cow::Borrowed(verdict),
                    platform,
                ),
                BulkOutcome::PerUnit => {
                    // A cue set: the whole-image lookup, then per-track
                    // reconciliation, exactly as the per-unit flow resolves it.
                    let digests = first
                        .digests
                        .as_ref()
                        .expect("a cue set digested successfully");
                    let data = match resolve_unit(
                        "organize",
                        &client,
                        &first.target,
                        &first.name,
                        digests,
                        cancel,
                    )
                    .await
                    {
                        Ok(data) => data,
                        Err(err) => {
                            let err = anyhow::Error::from(err);
                            if is_cancelled_error(&err) {
                                return Err(err);
                            }
                            degrade(&mut plans[index], &err, &primary, progress);
                            continue;
                        }
                    };
                    let platform = data.platform.clone();
                    (
                        data.matched,
                        std::borrow::Cow::Owned(data.verdict),
                        platform,
                    )
                }
            };
            // Staged members already carry the full digest set from
            // planning; only plain tierable inputs escalate here.
            if verdict == DatVerdict::Hint.as_str()
                && let Some(full) = &first.full
            {
                let target = first.target.clone();
                let digests = match digest_unit(&target, full, cache, progress, cancel).await {
                    Ok(digests) => digests,
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        if is_cancelled_error(&err) {
                            return Err(err);
                        }
                        degrade(&mut plans[index], &err, &primary, progress);
                        continue;
                    }
                };
                let data =
                    match resolve_unit("organize", &client, &target, &first.name, &digests, cancel)
                        .await
                    {
                        Ok(data) => data,
                        Err(err) => {
                            let err = anyhow::Error::from(err);
                            if is_cancelled_error(&err) {
                                return Err(err);
                            }
                            degrade(&mut plans[index], &err, &primary, progress);
                            continue;
                        }
                    };
                matched = data.matched;
                platform = data.platform;
                verdict = std::borrow::Cow::Owned(data.verdict);
            }
            let plan = &mut plans[index];
            if let Some(verified) = matched
                .as_ref()
                .filter(|m| m.game_match_type.is_hash_verified())
            {
                // The unit matched as it sits on disk: no strip is forced.
                accept_match(plan, verified);
            } else if verdict == DatVerdict::Unknown.as_str()
                && (plan.header.is_some() || plan.trim.is_some())
            {
                let header_len = plan.header.as_ref().map(|header| header.len);
                let trim_padded = plan.trim.as_ref().map(|trim| trim.padded_size);
                let variants = match retries[pos].take() {
                    Some(variants) => variants,
                    None => {
                        let source = primary.clone();
                        let worker_cancel = cancel.clone();
                        match tokio::task::spawn_blocking(move || {
                            retry_digests(&source, header_len, trim_padded, &worker_cancel)
                        })
                        .await
                        {
                            Ok(variants) => variants,
                            Err(err) => {
                                Err(anyhow::Error::new(err).context("headerless/padded digests"))
                            }
                        }
                    }
                };
                let variants = match variants {
                    Ok(variants) => variants,
                    Err(err) if is_cancelled_error(&err) || cancel.is_cancelled() => {
                        return Err(err);
                    }
                    Err(err) => {
                        degrade(plan, &err, &primary, progress);
                        continue;
                    }
                };
                for variant in &variants {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    let search = GameFileMatchSearch::from_digests(&first.name, &variant.digests);
                    match client.identify_relations(&search, cancel).await {
                        Ok(retry) if retry.game_match_type.is_hash_verified() => {
                            accept_retry_match(plan, &retry, variant);
                            platform = retry.platform.as_ref().map(|p| p.name.clone());
                            matched = Some(retry);
                            break;
                        }
                        Ok(_) => {}
                        Err(err) => {
                            let err = anyhow::Error::from(err);
                            if is_cancelled_error(&err) {
                                return Err(err);
                            }
                            degrade(plan, &err, &primary, progress);
                            break;
                        }
                    }
                }
            }
            naming[index] = DatNaming {
                candidate: rename_candidate(primary, matched.as_ref()),
                platform,
            };
        }
    }
    Ok(naming)
}

/// Builds the page's bulk identify request: one item per single-stream
/// first pass, in page order. The results pair back by position: the
/// k-th single owns the k-th result.
fn bulk_items(firsts: &[FirstPass]) -> Vec<BulkIdentifyItem> {
    let mut items = Vec::new();
    for first in firsts {
        let Ok(RomDigests::Single(digests)) = first.digests.as_ref() else {
            continue;
        };
        items.push(BulkIdentifyItem {
            search: GameFileMatchSearch::from_digests(&first.name, digests),
            key: None,
        });
    }
    items
}

/// The batched identify's outcome for one page unit, before the per-unit
/// escalation and cue-set reconciliation run.
enum BulkOutcome<'a> {
    /// The unit's digests failed: degrade with the error unless it carries
    /// a cancellation.
    DigestError(&'a anyhow::Error),
    /// The unit's bulk lookup failed: the page request errored, or the
    /// item answered with a non-Ok status. Carries the warning detail.
    BulkError(String),
    /// The unit's bulk item answered.
    Bulk {
        matched: Option<Box<GameAndRelationMatchResult>>,
        verdict: &'static str,
        platform: Option<String>,
    },
    /// A cue set (Tracks digests): not batched, resolve per unit.
    PerUnit,
}

/// Pairs the bulk results back onto the page: single-stream units own the
/// results in page order, digest failures and cue sets never pair.
/// `results` of `None` marks a failed page request whose `page_error`
/// degrades every owner.
fn bulk_outcomes<'a>(
    firsts: &'a [FirstPass],
    results: Option<Vec<BulkIdentifyRelationsResult>>,
    page_error: Option<&anyhow::Error>,
) -> Vec<BulkOutcome<'a>> {
    if let Some(results) = &results {
        debug_assert_eq!(
            results.len(),
            firsts
                .iter()
                .filter(|first| matches!(first.digests, Ok(RomDigests::Single(_))))
                .count()
        );
    }
    // The singles consume their results in page order, so the cursor
    // reconstructs the pairing without an owners list.
    let mut results = results.map(Vec::into_iter);
    let mut outcomes = Vec::with_capacity(firsts.len());
    let mut slot = 0usize;
    for first in firsts {
        let outcome = match first.digests.as_ref() {
            Err(err) => BulkOutcome::DigestError(err),
            Ok(RomDigests::Tracks { .. }) => BulkOutcome::PerUnit,
            Ok(RomDigests::Single(_)) => {
                let this = slot;
                slot += 1;
                match results.as_mut() {
                    Some(results) => match results.next() {
                        Some(result) if result.index == this => {
                            if result.status != BulkItemStatus::Ok {
                                let detail = match &result.error {
                                    Some(error) => format!("{}: {}", error.code, error.message),
                                    None => "no result".to_string(),
                                };
                                BulkOutcome::BulkError(detail)
                            } else {
                                let matched = result.matched.map(Box::new);
                                let verdict = match matched
                                    .as_deref()
                                    .map(|m| match_strength(m.game_match_type))
                                {
                                    Some(MatchStrength::Verified(_)) => {
                                        DatVerdict::Verified.as_str()
                                    }
                                    Some(MatchStrength::NameSizeHint) => DatVerdict::Hint.as_str(),
                                    Some(MatchStrength::NoMatch) | None => {
                                        DatVerdict::Unknown.as_str()
                                    }
                                };
                                let platform = matched
                                    .as_deref()
                                    .and_then(|m| m.platform.as_ref().map(|p| p.name.clone()));
                                BulkOutcome::Bulk {
                                    matched,
                                    verdict,
                                    platform,
                                }
                            }
                        }
                        Some(_) => BulkOutcome::BulkError("bulk results out of order".to_string()),
                        None => BulkOutcome::BulkError("bulk result missing".to_string()),
                    },
                    None => BulkOutcome::BulkError(
                        page_error
                            .expect("a failed page request carries its error")
                            .to_string(),
                    ),
                }
            }
        };
        outcomes.push(outcome);
    }
    outcomes
}

/// Records a DAT degrade on the plan: the unit falls back to keep-name,
/// clean suspends for the run, and the reason is warned.
fn degrade(
    plan: &mut UnitPlan,
    err: impl std::fmt::Display,
    primary: &Path,
    progress: &dyn ProgressReporter,
) {
    plan.match_error = Some(err.to_string());
    progress.warn(&format!(
        "DAT match failed for {}: {err}",
        primary.display()
    ));
}

/// Adopts a hash-verified match into the plan and applies the matched DAT's
/// header policy. The unit matched as it sits on disk, so no strip is
/// applied here; a strip is only ever set by a verified headerless-retry
/// match.
fn accept_match(plan: &mut UnitPlan, matched: &GameAndRelationMatchResult) {
    if let Some(game) = &matched.game {
        plan.game = Some(GameRef {
            id: game.id.clone(),
            parent_id: game.clone_of.clone(),
            name: game.name.clone(),
            dat_name: matched.dat_file.as_ref().map(|file| file.name.clone()),
            platform: matched.platform.as_ref().map(|p| p.name.clone()),
            tags: GameTags::parse(&game.name),
        });
    }
    apply_game_tokens(plan);
    apply_dat_header_policy(plan, matched);
}

/// Adopts a verified retry match: the headerless variant's hit strips the
/// header, the padded variant's hit records the fill byte that matched so
/// placement pads identically.
fn accept_retry_match(
    plan: &mut UnitPlan,
    matched: &GameAndRelationMatchResult,
    variant: &RetryVariant,
) {
    if variant.headerless {
        plan.strip = plan.header.as_ref().map_or(0, |header| header.len);
    } else {
        plan.pad_fill = variant.fill;
    }
    accept_match(plan, matched);
}

/// Copies the matched DAT entry's naming tokens onto the plan: `{region}`,
/// `{language}`, `{type}`, `{dat}`, and `{game}`. The header-detected region
/// only survives when the DAT tags none.
fn apply_game_tokens(plan: &mut UnitPlan) {
    let Some(game) = plan.game.as_ref() else {
        return;
    };
    let region = game.tags.primary_region().map(str::to_string);
    let language = game.tags.primary_language().map(str::to_string);
    let game_type = game.tags.type_label().to_string();
    let previous_region = plan.tokens.region.take();
    plan.tokens.region = region.or(previous_region);
    plan.tokens.language = language;
    plan.tokens.game_type = Some(game_type);
    plan.tokens.dat = game.dat_name.clone();
    plan.tokens.game = Some(game.name.clone());
}

/// Applies the matched DAT's header policy: a "headered" DAT undoes an
/// already-applied strip, since the unit matched with its header intact.
/// The strip itself is only ever set by a verified headerless-retry match,
/// which sets it directly.
fn apply_dat_header_policy(plan: &mut UnitPlan, matched: &GameAndRelationMatchResult) {
    if plan.header.is_none() {
        return;
    }
    let Some(dat_name) = matched
        .dat_file
        .as_ref()
        .map(|file| file.name.to_ascii_lowercase())
    else {
        return;
    };
    if dat_name.contains("headered") && plan.strip > 0 {
        plan.strip = 0;
    }
}

/// One headerless/padded digest variant to retry a match with. `headerless`
/// marks the header-stripped form, whose verified match forces the strip;
/// `fill` is the pad byte of a padded form (0 on the headerless form).
pub(super) struct RetryVariant {
    digests: FileDigests,
    headerless: bool,
    fill: u8,
}

/// Computes all requested staged-member digests and any headerless/padded
/// retries while the plan's extraction is still alive. Digest failures are
/// carried into matching; only cancellation aborts planning.
#[allow(clippy::too_many_arguments)]
pub(super) async fn digest_staged(
    archive: &Path,
    member: &Path,
    basis: &Path,
    bounds: &crate::util::ChecksumBounds,
    header_len: Option<u64>,
    trim_padded: Option<u64>,
    cache: Option<&HashCache>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<StagedDigests> {
    let requested = [HashAlgo::Crc32, HashAlgo::Sha1];
    let (floor, escalation) = bounds.split(&requested);
    let algos: Vec<HashAlgo> = floor.into_iter().chain(escalation).collect();
    // The sole disc or container image member of an archive shares the
    // hash-cache entry the `dat` commands key by the archive (they digest
    // that same member), so a warm run skips the digest. The caller passes
    // no cache for multi-member archives, and other members stay uncached:
    // `dat` resolves no member for those archives and must not find one.
    let cache = cache.filter(|_| {
        member
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ARCHIVE_IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
    });
    let digests = match cache.and_then(|cache| cache.lookup_decoded(archive, &algos)) {
        Some(hit) => Ok(RomDigests::Single(hit)),
        None => {
            let unit = DatUnit::File(member.to_path_buf());
            match digest_unit(&unit, &algos, None, progress, cancel).await {
                Ok(digests) => {
                    if let (Some(cache), RomDigests::Single(single)) = (cache, &digests) {
                        cache.store_decoded(archive, single);
                    }
                    Ok(digests)
                }
                Err(err) => {
                    // No extra context: the row warning prints only the
                    // outermost message, and the digest error names the cause.
                    let err = anyhow::Error::from(err);
                    if is_cancelled_error(&err) || cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    Err(err)
                }
            }
        }
    };
    let retry =
        if header_len.is_some() || trim_padded.is_some() {
            let member = member.to_path_buf();
            let basis = basis.display().to_string();
            let worker_cancel = cancel.clone();
            let result = tokio::task::spawn_blocking(move || {
                retry_digests(&member, header_len, trim_padded, &worker_cancel)
            })
            .await;
            Some(match result {
                Ok(Ok(variants)) => Ok(variants),
                Ok(Err(err)) if is_cancelled_error(&err) || cancel.is_cancelled() => {
                    return Err(Cancelled.into());
                }
                Ok(Err(err)) => Err(err.context(format!("headerless/padded digests for {basis}"))),
                Err(err) => Err(anyhow::Error::new(err)
                    .context(format!("headerless/padded digests for {basis}"))),
            })
        } else {
            None
        };
    Ok(StagedDigests { digests, retry })
}

/// The digest variants to retry matching with: the headerless form first,
/// then the padded form under each known fill byte. One streaming pass per
/// variant over the source path, never touching the hash cache.
fn retry_digests(
    source: &Path,
    header_len: Option<u64>,
    trim_padded: Option<u64>,
    cancel: &CancelToken,
) -> Result<Vec<RetryVariant>> {
    let size = std::fs::metadata(source)?.len();
    let mut variants = Vec::new();
    if let Some(len) = header_len {
        variants.push(RetryVariant {
            digests: stream_digests(source, len, None, size, cancel)?,
            headerless: true,
            fill: 0,
        });
    }
    if let Some(padded_size) = trim_padded {
        let pad = padded_size.saturating_sub(size);
        if pad > 0 {
            // The padded target may use either fill byte.
            for fill in [0x00u8, 0xFF] {
                variants.push(RetryVariant {
                    digests: stream_digests(source, 0, Some((pad, fill)), size, cancel)?,
                    headerless: false,
                    fill,
                });
            }
        }
    }
    Ok(variants)
}

/// CRC32+SHA1 over `source` after skipping `skip` leading bytes and
/// appending `pad` bytes of `fill`: one streaming pass, no cache.
fn stream_digests(
    source: &Path,
    skip: u64,
    pad: Option<(u64, u8)>,
    size: u64,
    cancel: &CancelToken,
) -> Result<FileDigests> {
    let (pad, fill) = pad.unwrap_or((0, 0));
    let mut file = std::fs::File::open(source)?;
    let mut hasher = MultiHasher::new(&[HashAlgo::Crc32, HashAlgo::Sha1]);
    pump_skip_pad(&mut file, &mut hasher, skip, pad, fill, &NoProgress, cancel)?;
    Ok(hasher.finalize(size.saturating_sub(skip) + pad))
}

/// Applies the DAT naming to the surviving Keep plans: the rename stem from
/// planning every candidate at once (so collision and disc-set guards see
/// the whole library), and the match's platform label for units whose
/// console resolved to nothing or a generic CHD/CSO label. Patched variants
/// keep their patch stem but take the same platform fallback.
pub(super) fn apply_dat_naming(plans: &mut [UnitPlan], naming: &[DatNaming]) {
    let candidates: Vec<RenameCandidate> = plans
        .iter()
        .filter(|plan| plan.decision == Decision::Keep && plan.patch.is_none())
        .map(|plan| naming[plan.index].candidate.clone())
        .collect();
    let stems: HashMap<PathBuf, Option<String>> = plan_renames(&candidates)
        .into_iter()
        .map(|plan| {
            let stem = (plan.action == RenameAction::Rename)
                .then_some(plan.to)
                .flatten()
                .and_then(|to| to.file_stem().map(|s| s.to_string_lossy().into_owned()));
            (plan.from, stem)
        })
        .collect();
    for plan in plans.iter_mut() {
        if plan.decision != Decision::Keep {
            continue;
        }
        let naming = &naming[plan.index];
        // Patched variants keep their patch stem; only base plans rename.
        if plan.patch.is_none()
            && let Some(stem) = stems.get(&naming.candidate.path).cloned().flatten()
        {
            plan.tokens.basename = stem;
        }
        if plan
            .tokens
            .console
            .as_deref()
            .is_none_or(|label| label == "CHD" || label == "CSO")
            && let Some(platform) = naming.platform.as_deref()
        {
            // No source extension refines a DAT platform label, so the
            // frontend label is the platform itself.
            super::plan::set_console(&mut plan.tokens, Some(platform), "");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::headers;
    use super::super::plan::Action;
    use super::*;
    use crate::dat::model::{
        BulkIdentifyRelationsResult, BulkItemError, GameMatchType, PlaymatchDatFile, PlaymatchGame,
        PlaymatchPlatform,
    };
    use crate::runner::models::{RunOptions, RunRequest};
    use crate::util::TemplateTokens;
    use std::path::Path;

    fn plan_with_action(index: usize, action: Action) -> UnitPlan {
        UnitPlan {
            index,
            source: PathBuf::from("Game.smc"),
            source_ext: "smc".to_string(),
            action,
            tokens: TemplateTokens::new(None, Path::new("Game.smc"), "zip"),
            input_subdir: PathBuf::new(),
            game: None,
            header: None,
            strip: 0,
            trim: None,
            pad: 0,
            pad_fill: 0x00,
            patch: None,
            staging_error: None,
            output_error: None,
            match_error: None,
            member: None,
            desired: None,
            decision: Decision::Keep,
        }
    }

    fn headerless_match() -> GameAndRelationMatchResult {
        GameAndRelationMatchResult {
            game_match_type: GameMatchType::Sha256,
            game: None,
            platform: None,
            company: None,
            signature_group: None,
            dat_file: Some(PlaymatchDatFile {
                id: "1".to_string(),
                name: "Game (USA) (Headerless)".to_string(),
                platform_id: "1".to_string(),
                signature_group_id: "1".to_string(),
                current_version: "20260101".to_string(),
                subset: None,
                tags: None,
            }),
            dat_file_import: None,
            external_metadata: Vec::new(),
            game_files: Vec::new(),
        }
    }

    /// A first-pass match adopts the DAT's tokens but never strips: the
    /// unit matched as it sits on disk, so stripping would destroy ROM
    /// bytes even when the DAT name says headerless.
    #[test]
    fn first_pass_match_never_strips() {
        let matched = headerless_match();
        let mut plan = plan_with_action(0, Action::Zip);
        plan.header = Some(headers::RomHeader {
            kind: "SMC",
            len: 512,
            headerless_ext: Some("sfc"),
        });
        accept_match(&mut plan, &matched);
        assert_eq!(plan.strip, 0);
    }

    /// A "headered" DAT undoes an already-applied strip: the unit matched
    /// with its header intact, so stripping it would lose those bytes.
    #[test]
    fn dat_header_policy_headered_dat_reverts_strip() {
        let mut matched = headerless_match();
        if let Some(dat_file) = matched.dat_file.as_mut() {
            dat_file.name = "Game (USA) (Headered)".to_string();
        }
        let mut plan = plan_with_action(0, Action::Zip);
        plan.header = Some(headers::RomHeader {
            kind: "SMC",
            len: 512,
            headerless_ext: Some("sfc"),
        });
        plan.strip = 512;

        apply_dat_header_policy(&mut plan, &matched);
        assert_eq!(plan.strip, 0);
    }

    /// A padded retry hit records the fill byte that verified, so the
    /// placement pads with the same byte the DAT entry describes; a
    /// headerless retry hit applies the strip and leaves the fill alone.
    #[test]
    fn retry_match_records_the_matching_fill_byte() {
        let matched = headerless_match();
        let header = || headers::RomHeader {
            kind: "SMC",
            len: 512,
            headerless_ext: Some("sfc"),
        };

        let mut padded = plan_with_action(0, Action::Zip);
        padded.header = Some(header());
        padded.pad = 4096;
        accept_retry_match(
            &mut padded,
            &matched,
            &RetryVariant {
                digests: FileDigests::default(),
                headerless: false,
                fill: 0xFF,
            },
        );
        assert_eq!(padded.pad_fill, 0xFF);
        assert_eq!(padded.strip, 0, "a padded hit forces no strip");

        let mut zero_padded = plan_with_action(0, Action::Zip);
        zero_padded.pad = 4096;
        zero_padded.pad_fill = 0xFF;
        accept_retry_match(
            &mut zero_padded,
            &matched,
            &RetryVariant {
                digests: FileDigests::default(),
                headerless: false,
                fill: 0x00,
            },
        );
        assert_eq!(
            zero_padded.pad_fill, 0x00,
            "a verified zero fill replaces 0xFF"
        );

        let mut headerless = plan_with_action(0, Action::Zip);
        headerless.header = Some(header());
        accept_retry_match(
            &mut headerless,
            &matched,
            &RetryVariant {
                digests: FileDigests::default(),
                headerless: true,
                fill: 0,
            },
        );
        assert_eq!(headerless.strip, 512);
        assert_eq!(headerless.pad_fill, 0x00);
    }

    /// Patched variants keep their patch stem but take the match's platform
    /// console fallback exactly like their base plan.
    #[test]
    fn patched_variants_take_the_platform_console_fallback() {
        let mut base = plan_with_action(0, Action::Copy);
        base.tokens.console = None;
        let mut patched = plan_with_action(0, Action::Copy);
        patched.tokens.basename = "Hack".to_string();
        patched.tokens.console = None;
        patched.patch = Some(PathBuf::from("Hack.ips"));

        let mut naming = vec![DatNaming::default(); 1];
        naming[0].candidate.path = PathBuf::from("Game.smc");
        naming[0].platform = Some("Sega Saturn".to_string());

        let mut plans = vec![base, patched];
        apply_dat_naming(&mut plans, &naming);

        assert_eq!(plans[0].tokens.basename, "Game");
        assert_eq!(plans[0].tokens.console.as_deref(), Some("Sega Saturn"));
        assert_eq!(
            plans[0].tokens.frontend_console.as_deref(),
            Some("Sega Saturn")
        );
        assert_eq!(plans[1].tokens.basename, "Hack");
        assert_eq!(plans[1].tokens.console.as_deref(), Some("Sega Saturn"));
        assert_eq!(
            plans[1].tokens.frontend_console.as_deref(),
            Some("Sega Saturn")
        );
    }

    const BODY: &[u8] = b"headerless rom body bytes 0123456789";

    /// CRC32+SHA1 of a byte slice via the shared single-pass helper, for
    /// cross-checking `stream_digests` variants.
    fn file_digests(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> FileDigests {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        stream_digests(&path, 0, None, bytes.len() as u64, &CancelToken::new()).unwrap()
    }

    #[test]
    fn headerless_digest_skips_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let mut rom = vec![0xEEu8; 512];
        rom.extend_from_slice(BODY);
        let path = dir.path().join("rom.sfc");
        std::fs::write(&path, &rom).unwrap();

        let stripped =
            stream_digests(&path, 512, None, rom.len() as u64, &CancelToken::new()).unwrap();
        let expected = file_digests(&dir, "body.bin", BODY);
        assert_eq!(stripped.crc32, expected.crc32);
        assert_eq!(stripped.sha1, expected.sha1);
        assert_eq!(stripped.size_bytes, BODY.len() as u64);
    }

    #[test]
    fn padded_digests_cover_both_fill_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rom.nds");
        std::fs::write(&path, BODY).unwrap();
        let padded_size = (BODY.len() as u64).next_power_of_two();
        let pad = padded_size - BODY.len() as u64;

        let zeros = stream_digests(
            &path,
            0,
            Some((pad, 0x00)),
            BODY.len() as u64,
            &CancelToken::new(),
        )
        .unwrap();
        let mut with_zeros = BODY.to_vec();
        with_zeros.resize(padded_size as usize, 0x00);
        let expected_zeros = file_digests(&dir, "zeros.bin", &with_zeros);
        assert_eq!(zeros.crc32, expected_zeros.crc32);
        assert_eq!(zeros.size_bytes, padded_size);

        let ffs = stream_digests(
            &path,
            0,
            Some((pad, 0xFF)),
            BODY.len() as u64,
            &CancelToken::new(),
        )
        .unwrap();
        let mut with_ffs = BODY.to_vec();
        with_ffs.resize(padded_size as usize, 0xFF);
        let expected_ffs = file_digests(&dir, "ffs.bin", &with_ffs);
        assert_eq!(ffs.crc32, expected_ffs.crc32);
        assert_eq!(ffs.size_bytes, padded_size);

        // The two fill bytes must produce different digests.
        assert_ne!(zeros.crc32, ffs.crc32);
    }

    // ---- batched bulk pairing ----

    fn single(size: u64) -> RomDigests {
        RomDigests::Single(FileDigests {
            size_bytes: size,
            ..Default::default()
        })
    }

    fn tracks() -> RomDigests {
        RomDigests::Tracks {
            tracks: Vec::new(),
            whole: FileDigests::default(),
        }
    }

    fn first_pass(name: &str, digests: anyhow::Result<RomDigests>) -> FirstPass {
        FirstPass {
            target: DatUnit::File(PathBuf::from(name)),
            name: name.to_string(),
            digests,
            full: None,
            retry: None,
        }
    }

    async fn digest_archive_plan(
        plan: &mut UnitPlan,
        archive: &Path,
        bounds: &crate::util::ChecksumBounds,
    ) {
        let archive = archive.to_path_buf();
        let staged_archive = archive.clone();
        let resolved = tokio::task::spawn_blocking(move || {
            crate::util::resolve_input(&staged_archive, crate::info::SUPPORTED_INFO_EXTENSIONS)
        })
        .await
        .unwrap()
        .unwrap();
        let basis = resolved.output_basis().to_path_buf();
        let digests = super::digest_staged(
            &archive,
            resolved.path(),
            &basis,
            bounds,
            plan.header.as_ref().map(|header| header.len),
            plan.trim.as_ref().map(|trim| trim.padded_size),
            None,
            &NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        plan.member = Some(super::super::plan::MemberFacts {
            basis,
            digests: Some(digests),
            crc: None,
        });
    }

    fn ok_result(
        index: usize,
        matched: Option<GameAndRelationMatchResult>,
    ) -> BulkIdentifyRelationsResult {
        BulkIdentifyRelationsResult {
            index,
            key: None,
            status: BulkItemStatus::Ok,
            matched,
            error: None,
        }
    }

    fn verified_match(platform: &str) -> GameAndRelationMatchResult {
        GameAndRelationMatchResult {
            game_match_type: GameMatchType::Sha1,
            game: Some(PlaymatchGame {
                id: "1".to_string(),
                name: "Some Game (USA)".to_string(),
                clone_of: None,
                current_in_latest_dat: true,
                last_seen_dat_version: None,
            }),
            platform: Some(PlaymatchPlatform {
                id: "1".to_string(),
                name: platform.to_string(),
            }),
            company: None,
            signature_group: None,
            dat_file: None,
            dat_file_import: None,
            game_files: Vec::new(),
            external_metadata: Vec::new(),
        }
    }

    /// A staged archive member carries plan-time first-pass and headerless
    /// digests into matching. The bulk lookup finds nothing, the retry
    /// request carries the headerless body's digest, and its verified answer
    /// strips the header and names the game.
    #[tokio::test]
    async fn archive_retry_uses_planned_member_digests_and_strips_on_verified_retry() {
        use std::io::Write as _;
        use std::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let body = vec![0xEEu8; 1024];
        let mut rom = vec![0u8; 512];
        rom.extend_from_slice(&body);
        let zip_path = dir.path().join("Game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Game.sfc", opts).unwrap();
        zip.write_all(&rom).unwrap();
        zip.finish().unwrap();
        let expected_sha1 = file_digests(&dir, "body.bin", &body).sha1.unwrap();

        let (query_tx, query_rx) = mpsc::channel::<String>();
        let server = crate::dat::client::mock_api::spawn(move |request_line, request_body| {
            if request_line.starts_with("POST /identify/bulk/relations") {
                let request: serde_json::Value = serde_json::from_slice(request_body).unwrap();
                let count = request["items"].as_array().unwrap().len();
                let results: Vec<_> = (0..count)
                    .map(|index| serde_json::json!({"index": index, "status": "ok", "match": null}))
                    .collect();
                serde_json::json!({
                    "summary": {
                        "total": count, "succeeded": count, "failed": 0,
                        "matched": 0, "unmatched": count
                    },
                    "results": results
                })
                .to_string()
            } else {
                let _ = query_tx.send(request_line.to_string());
                serde_json::json!({
                    "gameMatchType": "SHA1",
                    "game": {
                        "id": "g1",
                        "name": "Game (USA)",
                        "currentInLatestDat": true
                    }
                })
                .to_string()
            }
        });

        let req = RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(dir.path().to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                api_base: Some(server.url().to_string()),
                ..RunOptions::default()
            },
            dry_run: false,
            ctx: Default::default(),
        };
        let units = vec![DatUnit::File(zip_path.clone())];
        let mut plans = vec![plan_with_action(0, Action::Zip)];
        plans[0].header = Some(headers::RomHeader {
            kind: "SMC",
            len: 512,
            headerless_ext: Some("sfc"),
        });
        let bounds = crate::util::ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap();
        digest_archive_plan(&mut plans[0], &zip_path, &bounds).await;

        let naming = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            match_plans(
                &req,
                &units,
                &mut plans,
                &bounds,
                &NoProgress,
                &CancelToken::new(),
            ),
        )
        .await
        .expect("match_plans finished")
        .unwrap();
        drop(server);
        let retry_queries: Vec<String> = query_rx.into_iter().collect();

        assert_eq!(retry_queries.len(), 1, "{retry_queries:?}");
        assert!(
            retry_queries[0].contains(&format!("sha1={expected_sha1}")),
            "the retry carries the headerless body's digest: {}",
            retry_queries[0]
        );
        assert_eq!(plans[0].strip, 512, "a verified headerless retry strips");
        assert_eq!(
            plans[0].game.as_ref().map(|game| game.name.as_str()),
            Some("Game (USA)")
        );
        assert!(naming[0].candidate.verified);
    }

    /// A retry lookup that fails degrades the unit to keep-name and
    /// records the failure in `match_error` (clean suspends for the run).
    #[tokio::test]
    async fn failed_retry_lookup_records_match_error() {
        use std::io::Write as _;
        use std::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let body = vec![0xEEu8; 1024];
        let mut rom = vec![0u8; 512];
        rom.extend_from_slice(&body);
        let zip_path = dir.path().join("Game.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("Game.sfc", opts).unwrap();
        zip.write_all(&rom).unwrap();
        zip.finish().unwrap();

        // The bulk lookup finds nothing; the per-unit retry request gets a
        // body the client cannot parse, so the lookup fails.
        let (query_tx, query_rx) = mpsc::channel::<String>();
        let server = crate::dat::client::mock_api::spawn(move |request_line, request_body| {
            if request_line.starts_with("POST /identify/bulk/relations") {
                let request: serde_json::Value = serde_json::from_slice(request_body).unwrap();
                let count = request["items"].as_array().unwrap().len();
                let results: Vec<_> = (0..count)
                    .map(|index| serde_json::json!({"index": index, "status": "ok", "match": null}))
                    .collect();
                serde_json::json!({
                    "summary": {
                        "total": count, "succeeded": count, "failed": 0,
                        "matched": 0, "unmatched": count
                    },
                    "results": results
                })
                .to_string()
            } else {
                let _ = query_tx.send(request_line.to_string());
                "not json".to_string()
            }
        });

        let req = RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(dir.path().to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                api_base: Some(server.url().to_string()),
                ..RunOptions::default()
            },
            dry_run: false,
            ctx: Default::default(),
        };
        let units = vec![DatUnit::File(zip_path.clone())];
        let mut plans = vec![plan_with_action(0, Action::Zip)];
        plans[0].header = Some(headers::RomHeader {
            kind: "SMC",
            len: 512,
            headerless_ext: Some("sfc"),
        });

        let bounds = crate::util::ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap();
        digest_archive_plan(&mut plans[0], &zip_path, &bounds).await;
        let naming = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            match_plans(
                &req,
                &units,
                &mut plans,
                &bounds,
                &NoProgress,
                &CancelToken::new(),
            ),
        )
        .await
        .expect("match_plans finished")
        .unwrap();
        drop(server);

        assert!(
            query_rx.try_recv().is_ok(),
            "the retry lookup was attempted"
        );
        assert!(
            plans[0].match_error.is_some(),
            "the degrade records match_error"
        );
        assert_eq!(naming[0].candidate.path, zip_path, "keep-name degrade");
    }

    /// A surviving Bulk outcome's payload: the verdict, the platform label,
    /// and whether a match came through.
    fn bulk_payload<'a>(outcome: &'a BulkOutcome<'a>) -> (&'a str, Option<&'a str>, bool) {
        let BulkOutcome::Bulk {
            matched,
            verdict,
            platform,
        } = outcome
        else {
            panic!("expected a bulk outcome");
        };
        (verdict, platform.as_deref(), matched.is_some())
    }

    /// Every single-stream unit pairs with its bulk result in page order,
    /// carrying the mapped verdict and platform label.
    #[test]
    fn bulk_pairing_single_stream() {
        let firsts = vec![
            first_pass("a.bin", Ok(single(100))),
            first_pass("b.bin", Ok(single(200))),
        ];
        let items = bulk_items(&firsts);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].search.file_name, "a.bin");
        assert_eq!(items[1].search.file_size, 200);

        let results = vec![
            ok_result(0, Some(verified_match("Sega Saturn"))),
            ok_result(1, None),
        ];
        let outcomes = bulk_outcomes(&firsts, Some(results), None);
        assert_eq!(
            bulk_payload(&outcomes[0]),
            (DatVerdict::Verified.as_str(), Some("Sega Saturn"), true)
        );
        // A no-match result stays a bulk outcome with the unknown verdict.
        assert_eq!(
            bulk_payload(&outcomes[1]),
            (DatVerdict::Unknown.as_str(), None, false)
        );
    }

    /// Cue sets (Tracks digests) stay per unit and never consume a bulk
    /// slot; the singles around them pair in page order, each carrying its
    /// own result's content.
    #[test]
    fn bulk_pairing_cue_set_mix() {
        let firsts = vec![
            first_pass("a.bin", Ok(single(100))),
            first_pass("game.cue", Ok(tracks())),
            first_pass("b.bin", Ok(single(200))),
        ];
        let items = bulk_items(&firsts);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].search.file_name, "a.bin");
        assert_eq!(items[1].search.file_name, "b.bin");

        let results = vec![
            ok_result(0, Some(verified_match("Sony PlayStation"))),
            ok_result(1, Some(verified_match("Sega Saturn"))),
        ];
        let outcomes = bulk_outcomes(&firsts, Some(results), None);
        assert_eq!(
            bulk_payload(&outcomes[0]),
            (
                DatVerdict::Verified.as_str(),
                Some("Sony PlayStation"),
                true
            )
        );
        assert!(matches!(outcomes[1], BulkOutcome::PerUnit));
        assert_eq!(
            bulk_payload(&outcomes[2]),
            (DatVerdict::Verified.as_str(), Some("Sega Saturn"), true)
        );
    }

    /// A digest failure degrades only its own unit and consumes no bulk
    /// slot: the singles after it still pair in page order, each carrying
    /// its own result's content.
    #[test]
    fn bulk_pairing_digest_error_mix() {
        let firsts = vec![
            first_pass("a.bin", Ok(single(100))),
            first_pass("broken.bin", Err(anyhow::anyhow!("read failed"))),
            first_pass("b.bin", Ok(single(200))),
        ];
        let items = bulk_items(&firsts);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].search.file_name, "a.bin");
        assert_eq!(items[1].search.file_name, "b.bin");

        let results = vec![
            ok_result(0, Some(verified_match("Nintendo Entertainment System"))),
            ok_result(1, Some(verified_match("Sega Saturn"))),
        ];
        let outcomes = bulk_outcomes(&firsts, Some(results), None);
        assert_eq!(
            bulk_payload(&outcomes[0]),
            (
                DatVerdict::Verified.as_str(),
                Some("Nintendo Entertainment System"),
                true
            )
        );
        let BulkOutcome::DigestError(err) = &outcomes[1] else {
            panic!("expected the digest error to degrade its unit");
        };
        assert_eq!(err.to_string(), "read failed");
        assert_eq!(
            bulk_payload(&outcomes[2]),
            (DatVerdict::Verified.as_str(), Some("Sega Saturn"), true)
        );
    }

    /// Bulk results that arrive out of order degrade their unit instead of
    /// pairing another unit's game onto it.
    #[test]
    fn bulk_pairing_out_of_order_results_degrade() {
        let firsts = vec![
            first_pass("a.bin", Ok(single(100))),
            first_pass("b.bin", Ok(single(200))),
        ];
        let items = bulk_items(&firsts);
        assert_eq!(items.len(), 2);

        // Each result claims the other unit's slot.
        let results = vec![
            ok_result(1, Some(verified_match("Sony PlayStation"))),
            ok_result(0, Some(verified_match("Sega Saturn"))),
        ];
        let outcomes = bulk_outcomes(&firsts, Some(results), None);
        for outcome in &outcomes {
            let BulkOutcome::BulkError(detail) = outcome else {
                panic!("expected the out-of-order result to degrade its unit");
            };
            assert_eq!(detail, "bulk results out of order");
        }
    }

    /// A failed page request degrades every single-stream unit with the
    /// request's error.
    #[test]
    fn bulk_pairing_failed_page_request() {
        let firsts = vec![
            first_pass("a.bin", Ok(single(100))),
            first_pass("b.bin", Ok(single(200))),
        ];
        let items = bulk_items(&firsts);
        assert_eq!(items.len(), 2);

        let page_error = anyhow::anyhow!("network down");
        let outcomes = bulk_outcomes(&firsts, None, Some(&page_error));
        for outcome in &outcomes {
            let BulkOutcome::BulkError(detail) = outcome else {
                panic!("expected the failed page to degrade every single-stream unit");
            };
            assert_eq!(detail, "network down");
        }
    }

    /// A non-Ok item status degrades just that unit with the item's error
    /// detail, prefixed by its code; its neighbours pair normally.
    #[test]
    fn bulk_pairing_per_item_error_status() {
        let firsts = vec![
            first_pass("a.bin", Ok(single(100))),
            first_pass("b.bin", Ok(single(200))),
        ];
        let items = bulk_items(&firsts);
        assert_eq!(items.len(), 2);

        let results = vec![
            BulkIdentifyRelationsResult {
                index: 0,
                key: None,
                status: BulkItemStatus::Error,
                matched: None,
                error: Some(BulkItemError {
                    code: "TOO_LARGE".to_string(),
                    message: "file too large".to_string(),
                    field: None,
                }),
            },
            ok_result(1, Some(verified_match("Sega Saturn"))),
        ];
        let outcomes = bulk_outcomes(&firsts, Some(results), None);
        let BulkOutcome::BulkError(detail) = &outcomes[0] else {
            panic!("expected the item error to degrade its unit");
        };
        assert_eq!(detail, "TOO_LARGE: file too large");
        assert_eq!(
            bulk_payload(&outcomes[1]),
            (DatVerdict::Verified.as_str(), Some("Sega Saturn"), true)
        );
    }

    // ---- degrade-path naming ----

    /// A degraded unit's keep-name candidate keeps its real path, so
    /// plan_renames still sees it: the disc-set guard keeps both discs on
    /// their local names instead of renaming the matched one alone.
    #[test]
    fn degraded_unit_still_fires_the_disc_set_guard() {
        let mut disc1 = plan_with_action(0, Action::Copy);
        disc1.tokens.basename = "Game (Disc 1)".to_string();
        disc1.tokens.console = None;
        let mut disc2 = plan_with_action(1, Action::Copy);
        disc2.tokens.basename = "Game (Disc 2)".to_string();
        disc2.tokens.console = None;

        // Disc 1 matched and verified; disc 2's lookup degraded to keep-name.
        let naming = vec![
            DatNaming {
                candidate: rename_candidate(
                    PathBuf::from("dir/Game (Disc 1).chd"),
                    Some(&verified_match("Sega Saturn")),
                ),
                platform: None,
            },
            DatNaming {
                candidate: rename_candidate(PathBuf::from("dir/Game (Disc 2).chd"), None),
                platform: None,
            },
        ];

        let mut plans = vec![disc1, disc2];
        apply_dat_naming(&mut plans, &naming);

        assert_eq!(plans[0].tokens.basename, "Game (Disc 1)");
        assert_eq!(plans[1].tokens.basename, "Game (Disc 2)");
    }

    /// An unreachable API degrades both discs to keep-name candidates that
    /// carry the unit's real path: both namings reach plan_renames, so the
    /// disc-set and collision guards still see the whole library, and no
    /// default (empty-path) candidate survives.
    #[tokio::test]
    async fn unreachable_api_keeps_real_path_candidates_for_both_discs() {
        let dir = tempfile::tempdir().unwrap();
        let disc1 = dir.path().join("Game (Disc 1).bin");
        let disc2 = dir.path().join("Game (Disc 2).bin");
        std::fs::write(&disc1, b"disc one").unwrap();
        std::fs::write(&disc2, b"disc two").unwrap();

        let req = RunRequest {
            schema: None,
            operation: "organize".to_string(),
            input: Some(dir.path().to_path_buf()),
            output: None,
            config: None,
            preset: None,
            options: RunOptions {
                api_base: Some("http://127.0.0.1:1".to_string()),
                ..RunOptions::default()
            },
            dry_run: false,
            ctx: Default::default(),
        };
        let units = vec![DatUnit::File(disc1.clone()), DatUnit::File(disc2.clone())];
        let mut plans = vec![
            plan_with_action(0, Action::Copy),
            plan_with_action(1, Action::Copy),
        ];

        let bounds = crate::util::ChecksumBounds::new(HashAlgo::Crc32, HashAlgo::Sha256).unwrap();
        let naming = match_plans(
            &req,
            &units,
            &mut plans,
            &bounds,
            &NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(naming.len(), 2);
        assert_eq!(naming[0].candidate.path, disc1);
        assert_eq!(naming[1].candidate.path, disc2);
        // plan_renames sees both real paths: one rename plan per candidate.
        let candidates: Vec<RenameCandidate> = naming
            .iter()
            .map(|naming| naming.candidate.clone())
            .collect();
        let rename_plans = plan_renames(&candidates);
        assert_eq!(
            rename_plans
                .iter()
                .map(|plan| plan.from.clone())
                .collect::<Vec<_>>(),
            vec![disc1, disc2]
        );
    }
}
