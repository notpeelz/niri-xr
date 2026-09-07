use clap::Parser;

#[derive(Parser)]
#[command(version, about)]
pub struct Args {
    #[arg(
        long = "wait-xr",
        help = "Wait for the XR runtime to become available instead of failing"
    )]
    pub wait_xr: bool,
}
