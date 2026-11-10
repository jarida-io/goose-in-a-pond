//! The single microphone owner for GIAP: one process, one input device, many subscribers.

pub mod cpal_device;
pub mod energy;
pub mod owner;
pub mod ring;
pub mod testing;

pub use cpal_device::CpalCapture;
pub use energy::MicEnergy;
pub use owner::{
    spawn, CaptureDevice, ExclusiveJob, MicCommand, MicHandle, MicReader, MicShared, MicState,
    OwnerView,
};
pub use ring::{Ring, CAPTURE_RATE_HZ};
