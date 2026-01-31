use trill_arg0::arg0_dispatch_or_else;
use trill_common::CliConfigOverrides;
use trill_mcp_server::run_main;

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|trill_linux_sandbox_exe| async move {
        run_main(trill_linux_sandbox_exe, CliConfigOverrides::default()).await?;
        Ok(())
    })
}
