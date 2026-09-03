use clap::Parser;

/// Print the installed binary's supported operations and formats as JSON
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
pub struct CapabilitiesCommand {}
