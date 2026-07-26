mod cargo;
mod crates_io;
mod node;
mod pip;
mod pypi;
mod uv;

use std::sync::Arc;

use super::ManagerAdapter;

pub(super) fn standard_adapters() -> Vec<Arc<dyn ManagerAdapter>> {
    let mut adapters: Vec<Arc<dyn ManagerAdapter>> = vec![
        Arc::new(cargo::CargoAdapter),
        Arc::new(pip::PipAdapter),
        Arc::new(uv::UvAdapter),
    ];
    adapters.extend(node::standard_adapters());
    adapters
}
