use std::net::UdpSocket;

use permissioned_blockchain::context::{
    crypto::DigestHash,
    ordered_multicast::{serialize, Variant},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Message(String);

impl DigestHash for Message {
    fn hash(&self, hasher: &mut permissioned_blockchain::context::crypto::Hasher) {
        hasher.update(&self.0)
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("client") {
        let socket = UdpSocket::bind("10.0.0.10:0").unwrap();
        socket.set_broadcast(true).unwrap();
        let message = serialize(&Message(String::from("hello")));
        socket.send_to(&message, "10.0.0.255:60004").unwrap();
        return;
    }
    let socket = UdpSocket::bind("10.0.0.255:60004").unwrap();
    let mut buf = vec![0; 1024];
    let (len, _) = socket.recv_from(&mut buf).unwrap();
    let message = Variant::new_half_sip_hash(0).deserialize::<Message>(&buf[..len]);
    println!("{message:?}");
}
