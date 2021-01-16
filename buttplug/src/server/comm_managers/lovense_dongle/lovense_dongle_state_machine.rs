use super::{lovense_dongle_device_impl::*, lovense_dongle_messages::*};
use crate::server::comm_managers::DeviceCommunicationEvent;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use async_trait::async_trait;
use futures::{select, FutureExt};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

// I found this hot dog on the ground at
// https://news.ycombinator.com/item?id=22752907 and dusted it off. It still
// tastes fine.
#[async_trait]
pub trait LovenseDongleState: std::fmt::Debug + Send {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>>;
}

#[derive(Debug)]
enum IncomingMessage {
  CommMgr(LovenseDeviceCommand),
  Dongle(LovenseDongleIncomingMessage),
  Device(OutgoingLovenseData),
  Disconnect,
}

#[derive(Debug)]
struct ChannelHub {
  comm_manager_incoming: Receiver<LovenseDeviceCommand>,
  dongle_outgoing: Sender<OutgoingLovenseData>,
  dongle_incoming: Receiver<LovenseDongleIncomingMessage>,
  event_outgoing: Sender<DeviceCommunicationEvent>,
  is_scanning: Arc<AtomicBool>
}

impl ChannelHub {
  pub fn new(
    comm_manager_incoming: Receiver<LovenseDeviceCommand>,
    dongle_outgoing: Sender<OutgoingLovenseData>,
    dongle_incoming: Receiver<LovenseDongleIncomingMessage>,
    event_outgoing: Sender<DeviceCommunicationEvent>,
    is_scanning: Arc<AtomicBool>
  ) -> Self {
    Self {
      comm_manager_incoming,
      dongle_outgoing,
      dongle_incoming,
      event_outgoing,
      is_scanning
    }
  }

  pub fn create_new_wait_for_dongle_state(self) -> Option<Box<dyn LovenseDongleState>> {
    Some(Box::new(LovenseDongleWaitForDongle::new(
      self.comm_manager_incoming,
      self.event_outgoing,
      self.is_scanning
    )))
  }

  pub async fn wait_for_input(&mut self) -> IncomingMessage {
    select! {
      comm_res = self.comm_manager_incoming.recv().fuse() => {
        match comm_res {
          Some(msg) => IncomingMessage::CommMgr(msg),
          None => {
            error!("Disconnect in comm manager channel, assuming shutdown or catastrophic error, exiting loop");
            IncomingMessage::Disconnect
          }
        }
      }
      dongle_res = self.dongle_incoming.recv().fuse() => {
        match dongle_res {
          Some(msg) => IncomingMessage::Dongle(msg),
          None => {
            error!("Disconnect in dongle channel, assuming shutdown or disconnect, exiting loop");
            IncomingMessage::Disconnect
          }
        }
      }
    }
  }

  pub async fn wait_for_device_input(
    &mut self,
    device_incoming: &mut Receiver<OutgoingLovenseData>,
  ) -> IncomingMessage {
    pin_mut!(device_incoming);
    select! {
      comm_res = self.comm_manager_incoming.recv().fuse() => {
        match comm_res {
          Some(msg) => IncomingMessage::CommMgr(msg),
          None => {
            error!("Disconnect in comm manager channel, assuming shutdown or catastrophic error, exiting loop");
            IncomingMessage::Disconnect
          }
        }
      }
      dongle_res = self.dongle_incoming.recv().fuse() => {
        match dongle_res {
          Some(msg) => IncomingMessage::Dongle(msg),
          None => {
            error!("Disconnect in dongle channel, assuming shutdown or disconnect, exiting loop");
            IncomingMessage::Disconnect
          }
        }
      }
      device_res = device_incoming.recv().fuse() => {
        match device_res {
          Some(msg) => IncomingMessage::Device(msg),
          None => {
            error!("Disconnect in device channel, assuming shutdown or disconnect, exiting loop");
            IncomingMessage::Disconnect
          }
        }
      }
    }
  }

  pub async fn send_output(&self, msg: OutgoingLovenseData) {
    self.dongle_outgoing.send(msg).await.unwrap();
  }

  pub async fn send_event(&self, msg: DeviceCommunicationEvent) {
    self.event_outgoing.send(msg).await.unwrap();
  }

  pub fn set_scanning_status(&self, is_scanning: bool) {
    self.is_scanning.store(is_scanning, Ordering::SeqCst);
  }
}

pub fn create_lovense_dongle_machine(
  event_outgoing: Sender<DeviceCommunicationEvent>,
  comm_incoming_receiver: Receiver<LovenseDeviceCommand>,
  is_scanning: Arc<AtomicBool>
) -> Box<dyn LovenseDongleState> {
    Box::new(LovenseDongleWaitForDongle::new(
      comm_incoming_receiver,
      event_outgoing,
      is_scanning,
    ))
  }

macro_rules! state_definition {
  ($name:ident) => {
    #[derive(Debug)]
    struct $name {
      hub: ChannelHub,
    }

    impl $name {
      pub fn new(hub: ChannelHub) -> Self {
        Self { hub }
      }
    }
  };
}

macro_rules! device_state_definition {
  ($name:ident) => {
    #[derive(Debug)]
    struct $name {
      hub: ChannelHub,
      device_id: String,
    }

    impl $name {
      pub fn new(hub: ChannelHub, device_id: String) -> Self {
        Self { hub, device_id }
      }
    }
  };
}

#[derive(Debug)]
struct LovenseDongleWaitForDongle {
  comm_receiver: Receiver<LovenseDeviceCommand>,
  event_sender: Sender<DeviceCommunicationEvent>,
  is_scanning: Arc<AtomicBool>
}

impl LovenseDongleWaitForDongle {
  pub fn new(
    comm_receiver: Receiver<LovenseDeviceCommand>,
    event_sender: Sender<DeviceCommunicationEvent>,
    is_scanning: Arc<AtomicBool>
  ) -> Self {
    Self {
      comm_receiver,
      event_sender,
      is_scanning
    }
  }
}

#[async_trait]
impl LovenseDongleState for LovenseDongleWaitForDongle {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("Running wait for dongle step");
    let mut should_scan = false;
    while let Some(msg) = self.comm_receiver.recv().await {
      match msg {
        LovenseDeviceCommand::DongleFound(sender, receiver) => {
          let hub = ChannelHub::new(
            self.comm_receiver,
            sender,
            receiver,
            self.event_sender.clone(),
            self.is_scanning
          );
          if should_scan {
            return Some(Box::new(LovenseDongleStartScanning::new(hub)));
          }
          return Some(Box::new(LovenseDongleIdle::new(hub)));
        }
        LovenseDeviceCommand::StartScanning => {
          should_scan = true;
        }
        LovenseDeviceCommand::StopScanning => {
          should_scan = false;
        }
      }
    }
    None
  }
}

state_definition!(LovenseDongleIdle);

#[async_trait]
impl LovenseDongleState for LovenseDongleIdle {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("Running idle step");

    // Check to see if any toy is already connected.
    let autoconnect_msg = LovenseDongleOutgoingMessage {
      func: LovenseDongleMessageFunc::Statuss,
      message_type: LovenseDongleMessageType::Toy,
      id: None,
      command: None,
      eager: None,
    };
    self
      .hub
      .send_output(OutgoingLovenseData::Message(autoconnect_msg))
      .await;

    // This sleep is REQUIRED. If we send too soon after this, the dongle locks up.
    futures_timer::Delay::new(std::time::Duration::from_millis(250)).await;

    loop {
      let msg = self.hub.wait_for_input().await;
      match msg {
        IncomingMessage::Dongle(device_msg) => match device_msg.func {
          LovenseDongleMessageFunc::IncomingStatus => {
            if let Some(incoming_data) = device_msg.data {
              if Some(LovenseDongleResultCode::DeviceConnectSuccess) == incoming_data.status {
                info!("Lovense dongle already connected to a device, registering in system.");
                return Some(Box::new(LovenseDongleDeviceLoop::new(
                  self.hub,
                  incoming_data.id.unwrap(),
                )));
              }
            }
          }
          _ => error!("Cannot handle dongle function {:?}", device_msg),
        },
        IncomingMessage::CommMgr(comm_msg) => match comm_msg {
          LovenseDeviceCommand::StartScanning => {
            return Some(Box::new(LovenseDongleStartScanning::new(self.hub)));
          }
          LovenseDeviceCommand::StopScanning => {
            return Some(Box::new(LovenseDongleStopScanning::new(self.hub)));
          }
          _ => {
            error!(
              "Unhandled comm manager message to lovense dongle: {:?}",
              comm_msg
            );
          }
        },
        IncomingMessage::Disconnect => {
          error!("Channel disconnect of some kind, returning to 'wait for dongle' state.");
          return self.hub.create_new_wait_for_dongle_state();
        }
        _ => {
          error!("Unhandled message to lovense dongle: {:?}", msg);
        }
      }
    }
  }
}

state_definition!(LovenseDongleStartScanning);

#[async_trait]
impl LovenseDongleState for LovenseDongleStartScanning {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("scanning for devices");

    let scan_msg = LovenseDongleOutgoingMessage {
      message_type: LovenseDongleMessageType::Toy,
      func: LovenseDongleMessageFunc::Search,
      eager: None,
      id: None,
      command: None,
    };
    self
      .hub
      .set_scanning_status(true);
    self
      .hub
      .send_output(OutgoingLovenseData::Message(scan_msg))
      .await;
    Some(Box::new(LovenseDongleScanning::new(self.hub)))
  }
}

state_definition!(LovenseDongleScanning);

#[async_trait]
impl LovenseDongleState for LovenseDongleScanning {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("scanning for devices");
    loop {
      let msg = self.hub.wait_for_input().await;
      match msg {
        IncomingMessage::CommMgr(comm_msg) => {
          error!("Not handling comm input: {:?}", comm_msg);
        }
        IncomingMessage::Dongle(device_msg) => {
          match device_msg.func {
            LovenseDongleMessageFunc::ToyData => {
              if let Some(data) = device_msg.data {
                return Some(Box::new(LovenseDongleStopScanningAndConnect::new(
                  self.hub,
                  data.id.unwrap(),
                )));
              } else if device_msg.result.is_some() {
                // emit and return to idle
                return Some(Box::new(LovenseDongleIdle::new(self.hub)));
              }
            }
            _ => error!("Cannot handle dongle function {:?}", device_msg),
          }
        }
        IncomingMessage::Disconnect => {
          error!("Channel disconnect of some kind, returning to 'wait for dongle' state.");
          return self.hub.create_new_wait_for_dongle_state();
        }
        _ => error!("Cannot handle dongle function {:?}", msg),
      }
    }
  }
}

state_definition!(LovenseDongleStopScanning);

#[async_trait]
impl LovenseDongleState for LovenseDongleStopScanning {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("stopping search");
    let scan_msg = LovenseDongleOutgoingMessage {
      message_type: LovenseDongleMessageType::USB,
      func: LovenseDongleMessageFunc::StopSearch,
      eager: None,
      id: None,
      command: None,
    };
    self
      .hub
      .send_output(OutgoingLovenseData::Message(scan_msg))
      .await;
    self
      .hub
      .set_scanning_status(false);
    self
      .hub
      .send_event(DeviceCommunicationEvent::ScanningFinished)
      .await;
    None
  }
}

device_state_definition!(LovenseDongleStopScanningAndConnect);

#[async_trait]
impl LovenseDongleState for LovenseDongleStopScanningAndConnect {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("stopping search and connecting to device");
    let scan_msg = LovenseDongleOutgoingMessage {
      message_type: LovenseDongleMessageType::USB,
      func: LovenseDongleMessageFunc::StopSearch,
      eager: None,
      id: None,
      command: None,
    };
    self
      .hub
      .send_output(OutgoingLovenseData::Message(scan_msg))
      .await;
    loop {
      let msg = self.hub.wait_for_input().await;
      match msg {
        IncomingMessage::Dongle(device_msg) => match device_msg.func {
          LovenseDongleMessageFunc::Search => {
            if let Some(result) = device_msg.result {
              if result == LovenseDongleResultCode::SearchStopped {
                break;
              }
            }
          }
          _ => error!("Cannot handle dongle function {:?}", device_msg),
        },
        IncomingMessage::Disconnect => {
          error!("Channel disconnect of some kind, returning to 'wait for dongle' state.");
          return self.hub.create_new_wait_for_dongle_state();
        }
        _ => error!("Cannot handle dongle function {:?}", msg),
      }
    } 
    self
      .hub
      .set_scanning_status(false);
    self
      .hub
      .send_event(DeviceCommunicationEvent::ScanningFinished)
      .await;
    Some(Box::new(LovenseDongleDeviceLoop::new(
      self.hub,
      self.device_id.clone(),
    )))
  }
}

device_state_definition!(LovenseDongleDeviceLoop);

#[async_trait]
impl LovenseDongleState for LovenseDongleDeviceLoop {
  async fn transition(mut self: Box<Self>) -> Option<Box<dyn LovenseDongleState>> {
    info!("Running Lovense Dongle Device Event Loop");
    let (device_write_sender, mut device_write_receiver) = channel(256);
    let (device_read_sender, device_read_receiver) = channel(256);
    self
      .hub
      .send_event(DeviceCommunicationEvent::DeviceFound(Box::new(
        LovenseDongleDeviceImplCreator::new(
          &self.device_id,
          device_write_sender,
          device_read_receiver,
        ),
      )))
      .await;
    loop {
      let msg = self
        .hub
        .wait_for_device_input(&mut device_write_receiver)
        .await;
      match msg {
        IncomingMessage::Device(device_msg) => {
          self.hub.send_output(device_msg).await;
        }
        IncomingMessage::Dongle(dongle_msg) => {
          match dongle_msg.func {
            LovenseDongleMessageFunc::IncomingStatus => {
              if let Some(data) = dongle_msg.data {
                if data.status == Some(LovenseDongleResultCode::DeviceDisconnected) {
                  // Device disconnected, emit and return to idle.
                  return Some(Box::new(LovenseDongleIdle::new(self.hub)));
                }
              }
            }
            _ => device_read_sender.send(dongle_msg).await.unwrap(),
          }
        }
        IncomingMessage::CommMgr(comm_msg) => match comm_msg {
          LovenseDeviceCommand::StartScanning => {
            self
              .hub
              .send_event(DeviceCommunicationEvent::ScanningFinished)
              .await;
          }
          LovenseDeviceCommand::StopScanning => {
            self
              .hub
              .send_event(DeviceCommunicationEvent::ScanningFinished)
              .await;
          }
          _ => error!(
            "Cannot handle communication manager function {:?}",
            comm_msg
          ),
        },
        IncomingMessage::Disconnect => {
          error!("Channel disconnect of some kind, returning to 'wait for dongle' state.");
          return self.hub.create_new_wait_for_dongle_state();
        }
      }
    }
  }
}
