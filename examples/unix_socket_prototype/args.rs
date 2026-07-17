use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Bake font into svg
    #[arg(long, default_value_t = false)]
    pub server: bool,
}
