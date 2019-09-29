use std::error::Error;
use std::fmt;
use super::client::ButtplugClientError;
use crate::core::messages::ButtplugMessageUnion;
use crate::server::server::ButtplugServer;

#[derive(Debug)]
pub struct ButtplugClientConnectorError {
    pub message: String,
}

impl fmt::Display for ButtplugClientConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Init Error: {}", self.message)
    }
}

impl Error for ButtplugClientConnectorError {
    fn description(&self) -> &str {
        self.message.as_str()
    }

    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

pub trait ButtplugClientConnector {
    fn connect(&mut self) -> Option<ButtplugClientConnectorError>;
    fn disconnect(&mut self) -> Option<ButtplugClientConnectorError>;
    fn send(&mut self, msg: &ButtplugMessageUnion) -> Result<ButtplugMessageUnion, ButtplugClientError>;
}

pub struct ButtplugEmbeddedClientConnector {
    server: Option<ButtplugServer>,
    server_name: String,
    max_ping_time: u32
}

impl ButtplugEmbeddedClientConnector {
    pub fn new(name: &str, max_ping_time: u32) -> ButtplugEmbeddedClientConnector {
        ButtplugEmbeddedClientConnector {
            server: None,
            server_name: name.to_string(),
            max_ping_time: max_ping_time
        }
    }
}

impl ButtplugClientConnector for ButtplugEmbeddedClientConnector {
    fn connect(&mut self) -> Option<ButtplugClientConnectorError> {
        self.server = Option::Some(ButtplugServer::new(&self.server_name, self.max_ping_time));
        None
    }

    fn disconnect(&mut self) -> Option<ButtplugClientConnectorError> {
        self.server = None;
        None
    }

    fn send(&mut self, msg: &ButtplugMessageUnion) -> Result<ButtplugMessageUnion, ButtplugClientError> {
        match self.server {
            Some (ref mut _s) => return _s.send_message(msg).map_err(|x| ButtplugClientError::ButtplugError(x)),
            None => return Result::Err(ButtplugClientError::ButtplugClientConnectorError(ButtplugClientConnectorError { message: "Client not connected to server.".to_string() }))
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::client::client::ButtplugClient;

    #[test]
    fn test_embedded_connector() {
        let mut client = ButtplugClient::new("Test Client");
        client.connect(ButtplugEmbeddedClientConnector::new("Test Server", 0));
        assert!(client.connected());
    }
}
