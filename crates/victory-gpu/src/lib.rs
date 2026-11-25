//! D3D12.X on host D3D12.
//!
//! D3D12.X records commands from code inlined into the title, which writes
//! packets into the command list while its put pointer is below a limit and
//! calls the list's method otherwise. Our lists keep the limits at zero, so
//! every command arrives here as a call, to be run on host D3D12. The
//! title's GPU addresses are CPU addresses (console memory is unified).
//!
//! [`device`] is the device object titles create, [`dxgi`] its adapter side,
//! [`resource`] heaps and resources, [`fence`] fences, [`queue`] command
//! queues, [`list`] command allocators and lists, [`descriptor`]
//! descriptor heaps, [`root`] root signatures, [`query`] query heaps,
//! [`signature`] command signatures, [`pipeline`] pipeline states,
//! [`frame`] frame pacing, [`object`] what all console graphics objects
//! share, [`console`] which console model the title sees. Planned:
//! `present` (swap chain).

#[cfg(all(windows, feature = "diagnostics"))]
mod capture;
pub mod console;
#[cfg(all(windows, feature = "diagnostics"))]
mod copystat;
pub mod descriptor;
pub mod device;
#[cfg(windows)]
mod ambient;
#[cfg(windows)]
mod draw;
pub mod dxgi;
pub mod fence;
pub mod frame;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod indirect;
#[cfg(windows)]
mod inplace;
pub mod list;
pub mod object;
pub mod pipeline;
#[cfg(windows)]
mod primitive;
pub mod query;
pub mod queue;
pub mod resource;
pub mod root;
#[cfg(windows)]
mod sampler;
#[cfg(windows)]
pub mod shader_cache;
#[cfg(windows)]
pub mod low;
#[cfg(windows)]
mod memory;
#[cfg(windows)]
mod recycle;
#[cfg(windows)]
mod shadow;
pub mod signature;
#[cfg(all(windows, feature = "diagnostics"))]
mod storetrace;
#[cfg(windows)]
mod texture;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
pub use host::on_failure;
#[cfg(windows)]
pub use inplace::{
    title_memory_allocated, title_memory_freeing, title_memory_released, title_memory_section, title_memory_section_gone,
    title_memory_view,
};
#[cfg(windows)]
pub use watch::{
    title_memory_aliased, title_memory_reading, title_memory_reading_at, title_memory_remapped, title_memory_remapping,
    title_memory_remapping_at, writing as kernel_will_write,
};

/// mimalloc as the process's allocator in builds without diagnostics (which
/// count every block on top of it, `copystat`): the `fast-alloc` feature.
#[cfg(all(feature = "fast-alloc", not(all(windows, feature = "diagnostics"))))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
