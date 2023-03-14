//! DBus system interfaces.

use std::error::Error;

use tokio::runtime::Builder;
use zbus::Connection;

use crate::dbus::logind::ManagerProxy;

#[allow(clippy::all)]
mod logind;

/// Shutdown the system.
pub fn shutdown() -> Result<(), Box<dyn Error>> {
    Builder::new_current_thread().enable_all().build()?.block_on(shutdown_async())?;
    Ok(())
}

/// Reboot the system.
pub fn reboot() -> Result<(), Box<dyn Error>> {
    Builder::new_current_thread().enable_all().build()?.block_on(reboot_async())?;
    Ok(())
}

/// Async handler for the shutdown call.
async fn shutdown_async() -> zbus::Result<()> {
    let connection = Connection::system().await?;
    let logind = ManagerProxy::new(&connection).await?;
    logind.power_off(false).await
}

/// Async handler for the reboot call.
async fn reboot_async() -> zbus::Result<()> {
    let connection = Connection::system().await?;
    let logind = ManagerProxy::new(&connection).await?;
    logind.reboot(false).await
}
