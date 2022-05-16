// Buttplug Rust Source Code File - See https://buttplug.io for more info.
//
// Copyright 2016-2022 Nonpolynomial Labs LLC. All rights reserved.
//
// Licensed under the BSD 3-Clause license. See LICENSE file in the project root
// for full license information.

use super::xinput_device_impl::XInputDeviceImplCreator;
use crate::{
  core::ButtplugResultFuture,
  server::device::{
    communication::{
    DeviceCommunicationEvent,
    DeviceCommunicationManager,
    DeviceCommunicationManagerBuilder,
    },
    hardware::HardwareEvent,
  },
  util::async_manager,
};
use futures::{future, FutureExt};
use futures_timer::Delay;
use std::{
  string::ToString,
  sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
  },
  time::Duration,
};
use tokio::sync::{broadcast, mpsc, Notify};

// 1-index this because we use it elsewhere for showing which controller is which.
#[derive(Debug, Display, Clone, Copy)]
#[repr(u8)]
pub enum XInputControllerIndex {
  XInputController1 = 0,
  XInputController2 = 1,
  XInputController3 = 2,
  XInputController4 = 3,
}

// Windows has a nice API for Plug n' Play. However, I am lazy and do not want
// to figure out how to get to it via Rust. So we're polling at 2hz and hoping
// no one decides to be cute and unplug/replug USB devices really fast or
// something.
#[derive(Default, Debug, Clone)]
pub(super) struct XInputConnectionTracker {
  connected_gamepads: Arc<AtomicU8>,
  check_running: Arc<AtomicBool>,
}

pub(super) fn create_address(index: XInputControllerIndex) -> String {
  index.to_string()
}

async fn check_gamepad_connectivity(
  connected_gamepads: Arc<AtomicU8>,
  check_running: Arc<AtomicBool>,
  sender: Option<broadcast::Sender<HardwareEvent>>,
) {
  check_running.store(true, Ordering::SeqCst);
  let handle = rusty_xinput::XInputHandle::load_default()
    .expect("Always loads in windows, this shouldn't run elsewhere.");
  loop {
    let gamepads = connected_gamepads.load(Ordering::SeqCst);
    if gamepads == 0 {
      break;
    }
    for index in &[
      XInputControllerIndex::XInputController1,
      XInputControllerIndex::XInputController2,
      XInputControllerIndex::XInputController3,
      XInputControllerIndex::XInputController4,
    ] {
      // If this isn't in our list of known gamepads, continue.
      if (gamepads & 1 << *index as u8) == 0 {
        continue;
      }
      // If we can't get state, assume we have disconnected.
      if handle.get_state(*index as u32).is_err() {
        info!("XInput gamepad {} has disconnected.", index);
        let new_connected_gamepads = gamepads & !(1 << *index as u8);
        connected_gamepads.store(new_connected_gamepads, Ordering::SeqCst);
        if let Some(send) = &sender {
          send
            .send(HardwareEvent::Disconnected(create_address(*index)))
            .expect("Infallible, device manager listening or this doesn't exist.");
        }
        // If we're out of gamepads to track, return immediately.
        if new_connected_gamepads == 0 {
          check_running.store(false, Ordering::SeqCst);
          return;
        }
      }
    }
    Delay::new(Duration::from_millis(500)).await;
  }
}

impl XInputConnectionTracker {
  pub fn add(&self, index: XInputControllerIndex) {
    debug!("Adding XInput device {} to connection tracker.", index);
    let mut connected = self.connected_gamepads.load(Ordering::SeqCst);
    let should_start = connected == 0 && !self.check_running.load(Ordering::SeqCst);
    connected |= 1 << index as u8;
    self.connected_gamepads.store(connected, Ordering::SeqCst);
    if should_start {
      let connected_gamepads = self.connected_gamepads.clone();
      let check_running = self.check_running.clone();
      async_manager::spawn(async move {
        check_gamepad_connectivity(connected_gamepads, check_running, None).await;
      });
    }
  }

  pub fn add_with_sender(
    &self,
    index: XInputControllerIndex,
    sender: broadcast::Sender<HardwareEvent>,
  ) {
    let mut connected = self.connected_gamepads.load(Ordering::SeqCst);
    let should_start = connected == 0;
    connected |= 1 << index as u8;
    self.connected_gamepads.store(connected, Ordering::SeqCst);
    if should_start {
      let connected_gamepads = self.connected_gamepads.clone();
      let check_running = self.check_running.clone();
      async_manager::spawn(async move {
        check_gamepad_connectivity(connected_gamepads, check_running, Some(sender)).await;
      });
    }
  }

  pub fn connected(&self, index: XInputControllerIndex) -> bool {
    self.connected_gamepads.load(Ordering::SeqCst) & (1 << index as u8) > 0
  }
}

#[derive(Default, Clone)]
pub struct XInputDeviceCommunicationManagerBuilder {}

impl DeviceCommunicationManagerBuilder for XInputDeviceCommunicationManagerBuilder {
  fn finish(&self, sender: mpsc::Sender<DeviceCommunicationEvent>) -> Box<dyn DeviceCommunicationManager> {
    Box::new(XInputDeviceCommunicationManager::new(sender))
  }
}

pub struct XInputDeviceCommunicationManager {
  sender: mpsc::Sender<DeviceCommunicationEvent>,
  scanning_notifier: Arc<Notify>,
  connected_gamepads: Arc<XInputConnectionTracker>,
}

impl XInputDeviceCommunicationManager {
  fn new(sender: mpsc::Sender<DeviceCommunicationEvent>) -> Self {
    Self {
      sender,
      scanning_notifier: Arc::new(Notify::new()),
      connected_gamepads: Arc::new(XInputConnectionTracker::default()),
    }
  }
}

impl DeviceCommunicationManager for XInputDeviceCommunicationManager {
  fn name(&self) -> &'static str {
    "XInputDeviceCommunicationManager"
  }

  fn start_scanning(&self) -> ButtplugResultFuture {
    debug!("XInput manager scanning for devices");
    let sender = self.sender.clone();
    let scanning_notifier = self.scanning_notifier.clone();
    let connected_gamepads = self.connected_gamepads.clone();
    async_manager::spawn(async move {
      let handle = rusty_xinput::XInputHandle::load_default()
        .expect("Always loads in windows, this shouldn't run elsewhere.");
      let mut stop = false;
      while !stop {
        for i in &[
          XInputControllerIndex::XInputController1,
          XInputControllerIndex::XInputController2,
          XInputControllerIndex::XInputController3,
          XInputControllerIndex::XInputController4,
        ] {
          match handle.get_state(*i as u32) {
            Ok(_) => {
              let index = *i as u32;
              if connected_gamepads.connected(*i) {
                trace!("XInput device {} already found, ignoring.", *i);
                continue;
              }
              info!("XInput manager found device {}", index);
              let device_creator = Box::new(XInputDeviceImplCreator::new(*i));
              connected_gamepads.add(*i);
              if sender
                .send(DeviceCommunicationEvent::DeviceFound {
                  name: i.to_string(),
                  address: i.to_string(),
                  creator: device_creator,
                })
                .await
                .is_err()
              {
                error!("Error sending device found message from Xinput.");
                break;
              }
            }
            Err(_) => {
              continue;
            }
          }
        }
        // Wait for either one second, or until our notifier has been notified.
        select! {
          _ = Delay::new(Duration::from_secs(1)).fuse() => {},
          _ = scanning_notifier.notified().fuse() => {
            debug!("XInput stop scanning notifier notified, ending scanning loop");
            stop = true;
          }
        }
      }
    });
    Box::pin(future::ready(Ok(())))
  }

  fn stop_scanning(&self) -> ButtplugResultFuture {
    debug!("XInput device comm manager received Stop Scanning request");
    self.scanning_notifier.notify_waiters();
    let sender = self.sender.clone();
    Box::pin(async move {
      if sender
        .send(DeviceCommunicationEvent::ScanningFinished)
        .await
        .is_err()
      {
        error!("Error sending scanning finished from Xinput.");
      }
      Ok(())
    })
  }

  // We should always be able to at least look at xinput if we're up on windows.
  fn can_scan(&self) -> bool {
    true
  }
}