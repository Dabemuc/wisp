use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Start server
    #[arg(long, default_value_t = false)]
    pub server: bool,

    /// Kill server
    #[arg(long, default_value_t = false)]
    pub kill: bool,
}
