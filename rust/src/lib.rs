//! primitive-gpu as a library: the search (`model`), the GPU scoring
//! (`gpu`), the exact CPU math (`core`, `raster`), the shapes, and the
//! picture loading shared by the command-line program and the GUI.

pub mod constraints;
pub mod core;
pub mod gpu;
pub mod load;
pub mod model;
pub mod raster;
pub mod rng;
pub mod shapes;
