//! Distribution-specific updates; App Store builds never link Sparkle.
#[cfg(all(feature = "mac-app-store", feature = "direct-distribution"))]
compile_error!(
    "mac-app-store requires --no-default-features; Sparkle cannot ship in the store build"
);

#[cfg(not(any(feature = "mac-app-store", feature = "direct-distribution")))]
compile_error!("select a distribution channel: direct-distribution or mac-app-store");

#[cfg(feature = "direct-distribution")]
mod direct;
#[cfg(feature = "direct-distribution")]
pub use direct::Updater;

#[cfg(not(feature = "direct-distribution"))]
mod store;
#[cfg(not(feature = "direct-distribution"))]
pub use store::Updater;
