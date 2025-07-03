use crate::commands::ctr::CdnToCiaCommand;
use crate::nintendo::ctr::cia::{decrypt_from_encrypted_cia, write_cia};
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::nintendo::ctr::title_key::generate_title_key;
use crate::nintendo::ctr::util::fs::{find_title_file, find_tmd_file};
use anyhow::Result;
use binrw::BinRead;
use futures::TryFutureExt;
use log::{debug, info, warn};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};

mod cia;
mod constants;
mod decrypt;
pub mod models;
pub mod title_key;
mod util;

pub async fn decrypt_cia(input: &Path, output: &Path) -> Result<()> {
    let out = File::create(output).await?;
    let mut out = BufWriter::new(out);

    decrypt_from_encrypted_cia(input, &mut out).await?;

    out.flush().await?;

    info!("Successfully decrypted CIA file");

    Ok(())
}

pub async fn generate_ticket_from_cdn(cdn_dir: &Path, output: &Path) -> Result<()> {
    let tmd_path = find_tmd_file(cdn_dir).await?;
    debug!("Found TMD file");

    let mut ticket_metadata_data = Cursor::new(fs::read(&tmd_path).await?);
    let title_metadata = TitleMetadata::read(&mut ticket_metadata_data)?;

    let title_id_str = format!("{:016X}", title_metadata.header.title_id);

    let title_key = generate_title_key(&title_id_str, None)?;

    const CETK_STRING_TEMPLATE: &str = "00010004d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f742d434130303030303030332d585330303030303030630000000000000000000000000000000000000000000000000000000000000000000000000000feedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedface010000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee00000000000000000000000000dddddddddddddddd00001111000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010014000000ac000000140001001400000000000000280000000100000084000000840003000000000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010004919ebe464ad0f552cd1b72e7884910cf55a9f02e50789641d896683dc005bd0aea87079d8ac284c675065f74c8bf37c88044409502a022980bb8ad48383f6d28a79de39626ccb2b22a0f19e41032f094b39ff0133146dec8f6c1a9d55cd28d9e1c47b3d11f4f5426c2c780135a2775d3ca679bc7e834f0e0fb58e68860a71330fc95791793c8fba935a7a6908f229dee2a0ca6b9b23b12d495a6fe19d0d72648216878605a66538dbf376899905d3445fc5c727a0e13e0e2c8971c9cfa6c60678875732a4e75523d2f562f12aabd1573bf06c94054aefa81a71417af9a4a066d0ffc5ad64bab28b1ff60661f4437d49e1e0d9412eb4bcacf4cfd6a3408847982000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f742d43413030303030303033000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000158533030303030303063000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000137a0894ad505bb6c67e2e5bdd6a3bec43d910c772e9cc290da58588b77dcc11680bb3e29f4eabbb26e98c2601985c041bb14378e689181aad770568e928a2b98167ee3e10d072beef1fa22fa2aa3e13f11e1836a92a4281ef70aaf4e462998221c6fbb9bdd017e6ac590494e9cea9859ceb2d2a4c1766f2c33912c58f14a803e36fccdcccdc13fd7ae77c7a78d997e6acc35557e0d3e9eb64b43c92f4c50d67a602deb391b06661cd32880bd64912af1cbcb7162a06f02565d3b0ece4fcecddae8a4934db8ee67f3017986221155d131c6c3f09ab1945c206ac70c942b36f49a1183bcd78b6e4b47c6c5cac0f8d62f897c6953dd12f28b70c5b7df751819a9834652625000100010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010003704138efbbbda16a987dd901326d1c9459484c88a2861b91a312587ae70ef6237ec50e1032dc39dde89a96a8e859d76a98a6e7e36a0cfe352ca893058234ff833fcb3b03811e9f0dc0d9a52f8045b4b2f9411b67a51c44b5ef8ce77bd6d56ba75734a1856de6d4bed6d3a242c7c8791b3422375e5c779abf072f7695efa0f75bcb83789fc30e3fe4cc8392207840638949c7f688565f649b74d63d8d58ffadda571e9554426b1318fc468983d4c8a5628b06b6fc5d507c13e7a18ac1511eb6d62ea5448f83501447a9afb3ecc2903c9dd52f922ac9acdbef58c6021848d96e208732d3d1d9d9ea440d91621c7a99db8843c59c1f2e2c7d9b577d512c166d6f7e1aad4a774a37447e78fe2021e14a95d112a068ada019f463c7a55685aabb6888b9246483d18b9c806f474918331782344a4b8531334b26303263d9d2eb4f4bb99602b352f6ae4046c69a5e7e8e4a18ef9bc0a2ded61310417012fd824cc116cfb7c4c1f7ec7177a17446cbde96f3edd88fcd052f0b888a45fdaf2b631354f40d16e5fa9c2c4eda98e798d15e6046dc5363f3096b2c607a9d8dd55b1502a6ac7d3cc8d8c575998e7d796910c804c495235057e91ecd2637c9c1845151ac6b9a0490ae3ec6f47740a0db0ba36d075956cee7354ea3e9a4f2720b26550c7d394324bc0cb7e9317d8a8661f42191ff10b08256ce3fd25b745e5194906b4d61cb4c2e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f7400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001434130303030303030330000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000007be8ef6cb279c9e2eee121c6eaf44ff639f88f078b4b77ed9f9560b0358281b50e55ab721115a177703c7a30fe3ae9ef1c60bc1d974676b23a68cc04b198525bc968f11de2db50e4d9e7f071e562dae2092233e9d363f61dd7c19ff3a4a91e8f6553d471dd7b84b9f1b8ce7335f0f5540563a1eab83963e09be901011f99546361287020e9cc0dab487f140d6626a1836d27111f2068de4772149151cf69c61ba60ef9d949a0f71f5499f2d39ad28c7005348293c431ffbd33f6bca60dc7195ea2bcc56d200baf6d06d09c41db8de9c720154ca4832b69c08c69cd3b073a0063602f462d338061a5ea6c915cd5623579c3eb64ce44ef586d14baaa8834019b3eebeed3790001000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    let title_version_hex = format!("{:04x}", title_metadata.header.title_version);

    let cetk = CETK_STRING_TEMPLATE
        .replace("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", &title_key)
        .replace("1111", &title_version_hex)
        .replace("dddddddddddddddd", &title_id_str);

    let mut file = File::create(&output).await?;

    file.write_all(&hex::decode(cetk)?).await?;

    info!("✅ Successfully created Ticket at {}", output.display());

    Ok(())
}

pub async fn convert_cdn_to_cia(cmd: CdnToCiaCommand) -> Result<()> {
    if cmd.recursive {
        let mut directories = tokio::fs::read_dir(&cmd.cdn_dir).await?;

        while let Ok(Some(entry)) = directories.next_entry().await {
            debug!("Processing directory: {}", entry.path().display());

            if entry.path().is_file() {
                continue;
            }

            let mut cmd_clone = cmd.clone();
            cmd_clone.cdn_dir = entry.path();
            cmd_clone.output = None;

            if let Err(err) = convert_cdn_to_cia_single(cmd_clone).await {
                warn!(
                    "Failed to convert CDN directory {}: {}",
                    entry.path().display(),
                    err
                );
            }
        }

        Ok(())
    } else {
        convert_cdn_to_cia_single(cmd).await
    }
}

async fn convert_cdn_to_cia_single(cmd: CdnToCiaCommand) -> Result<()> {
    let output = cmd.output.unwrap_or_else(|| {
        let name = cmd
            .cdn_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}.cia"))
            .unwrap();

        let parent = cmd.cdn_dir.parent().unwrap_or_else(|| Path::new("."));
        parent.join(name)
    });

    let cdn_dir = &cmd.cdn_dir;

    let ticket_path = find_title_file(cdn_dir)
        .or_else(|err| async {
            if cmd.ensure_ticket_exists {
                let path = cdn_dir.join("ticket.tik");
                debug!("Path for ticket file: {}", path.display());
                debug!("CDN Directory: {}", cdn_dir.display());
                generate_ticket_from_cdn(cdn_dir, &path).await?;
                Ok::<PathBuf, anyhow::Error>(path)
            } else {
                Err(err.into())
            }
        })
        .await?;
    debug!("Found Ticket file");

    let title_metadata_path = find_tmd_file(cdn_dir).await?;
    debug!("Found TMD file");

    let mut ticket_metadata_data = Cursor::new(fs::read(&title_metadata_path).await?);
    let title_metadata = TitleMetadata::read(&mut ticket_metadata_data)?;

    let mut ticket_data = Cursor::new(fs::read(&ticket_path).await?);
    let ticket = Ticket::read(&mut ticket_data)?;

    debug!("Processing CIA conversion");

    let ticket_title_id = ticket.ticket_data.title_id;
    let title_metadata_title_id = title_metadata.header.title_id;

    if ticket_title_id != title_metadata_title_id {
        warn!(
            "warning: TICKET and TMD Title IDs do not match: TICKET=0x{ticket_title_id:016X}, TMD=0x{title_metadata_title_id:016X}"
        );
    }

    let out = File::create(&output).await?;
    let mut out_buffered = BufWriter::new(out);
    write_cia(
        cdn_dir,
        &mut out_buffered,
        &title_metadata_path,
        &ticket_path,
        title_metadata,
        ticket,
    )
    .await?;

    info!("✅ Successfully created CIA file {}", output.display());

    if cmd.decrypt {
        let decrypted_cia_path = output.with_extension("-decrypted.cia");

        decrypt_cia(&output, &decrypted_cia_path).await?;

        fs::remove_file(&output).await?;
        fs::rename(&decrypted_cia_path, &output).await?;

        debug!("Deleted original encrypted CIA file: {}", output.display());
    }

    if cmd.cleanup {
        fs::remove_dir_all(cdn_dir).await?;

        debug!("Deleted CDN directory: {}", cdn_dir.display());
    }

    Ok(())
}
