pub mod agents;
pub mod apps;
pub mod cgroup;
pub mod config;
pub mod dbus;
pub mod docker;
pub mod format;
pub mod history;
pub mod model;
pub mod notify;
pub mod orphans;
pub mod procscan;
pub mod roots;
pub mod sampler;
pub mod system;

pub const BUS_NAME: &str = "io.github.sirwilliamdev.Hivetop";
pub const OBJECT_PATH: &str = "/io/github/sirwilliamdev/Hivetop";
pub const INTERFACE: &str = "io.github.sirwilliamdev.Hivetop";

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
