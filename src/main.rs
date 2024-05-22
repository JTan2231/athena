use std::collections::HashMap;
use std::io::{self, stdin, Write};
use std::net::IpAddr;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

enum MessageType {
    Heartbeat = 0,
    Message = 1,
}

struct Peer {
    address: SocketAddr,
    last_seen: Instant,
}

struct Message {
    sender: SocketAddr,
    message_type: MessageType,
    payload_size: u16,
    payload: String,
}

// de/serialization for sending through the sockets
impl Message {
    fn serialize(&self) -> Vec<u8> {
        let mut serialized = Vec::new();
        if let IpAddr::V4(ip) = self.sender.ip() {
            serialized.extend(ip.octets().iter());
        } else {
            panic!("Only IPv4 addresses are supported");
        }

        serialized.extend(&self.sender.port().to_be_bytes());
        match self.message_type {
            MessageType::Heartbeat => serialized.push(MessageType::Heartbeat as u8),
            MessageType::Message => serialized.push(MessageType::Message as u8),
        }
        serialized.extend(self.payload_size.to_be_bytes());
        serialized.extend(self.payload.as_bytes());

        serialized
    }

    fn deserialize(data: &[u8]) -> Result<Message, String> {
        let ip = IpAddr::V4(std::net::Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        let port = u16::from_be_bytes([data[4], data[5]]);
        let sender = SocketAddr::new(ip, port);
        let message_type = match data[6] {
            0 => MessageType::Heartbeat,
            1 => MessageType::Message,
            _ => return Err("Invalid message type".to_string()),
        };
        let payload = String::from_utf8_lossy(&data[7..]);

        Ok(Message {
            sender,
            message_type,
            payload_size: payload.len() as u16,
            payload: payload.to_string(),
        })
    }
}

fn main() {
    // I'd like the usage for this as a CLI tool to be something like
    // `./hermes <listening_port> <target_port>

    // check for cli args
    if std::env::args().len() < 3 {
        eprintln!("Usage: ./hermes <listening_port> <target_port>");
        std::process::exit(1);
    }

    let listening_port = std::env::args().nth(1).unwrap_or("8080".to_string());
    let target_port = std::env::args().nth(2).unwrap_or("8081".to_string());

    let localhost = "127.0.0.1";

    let listening_address = format!("{}:{}", localhost, listening_port);
    let target_address = format!("{}:{}", localhost, target_port);

    let socket = UdpSocket::bind(listening_address.clone()).unwrap();
    println!("Listening on {}", listening_address);

    // hash map of addresses to peers
    let address_book: Arc<Mutex<HashMap<SocketAddr, Peer>>> = Arc::new(Mutex::new(HashMap::new()));

    // Starting a thread to handle incoming messages
    {
        let socket = socket.try_clone().unwrap();
        let address_book = Arc::clone(&address_book);
        thread::spawn(move || loop {
            let mut buf = [0; 1024];
            match socket.recv_from(&mut buf) {
                Ok((number_of_bytes, src_addr)) => {
                    let message = Message::deserialize(&buf[..number_of_bytes]);
                    match message {
                        Ok(message) => match message.message_type {
                            MessageType::Heartbeat => {
                                println!("Received heartbeat from {}", src_addr);

                                // update the address book
                                let mut address_book = address_book.lock().unwrap();
                                if let Some(peer) = address_book.get_mut(&src_addr) {
                                    peer.last_seen = Instant::now();
                                } else {
                                    address_book.insert(
                                        src_addr,
                                        Peer {
                                            address: src_addr,
                                            last_seen: Instant::now(),
                                        },
                                    );
                                }
                            }
                            MessageType::Message => {
                                println!("Received message from {}: {}", src_addr, message.payload);
                            }
                        },
                        Err(e) => {
                            eprintln!("Couldn't receive a datagram: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Couldn't receive a datagram: {}", e);
                }
            }
        });
    }

    // starting a thread for heartbeats
    {
        let socket = socket.try_clone().unwrap();
        let target_address = target_address.clone();
        let listening_address = listening_address.clone();
        thread::spawn(move || loop {
            // send a heartbeat to the target address
            let target_addr: SocketAddr = target_address.parse().expect("Invalid address format");

            let message = Message {
                sender: listening_address.parse().unwrap(),
                message_type: MessageType::Heartbeat,
                payload_size: 0,
                payload: "".to_string(),
            };

            socket
                .send_to(message.serialize().as_slice(), target_addr)
                .expect("Failed to send heartbeat");

            thread::sleep(std::time::Duration::from_secs(1));
        });
    }

    // Handling user input to send messages
    loop {
        let mut input = String::new();
        print!("> ");
        io::stdout().flush().unwrap();
        stdin().read_line(&mut input).unwrap();
        let trimmed_input = input.trim();

        if trimmed_input == ":quit" {
            break;
        }

        let message = Message {
            sender: listening_address.parse().unwrap(),
            message_type: MessageType::Message,
            payload_size: trimmed_input.len() as u16,
            payload: trimmed_input.to_string(),
        };

        let remote_addr: SocketAddr = target_address
            .clone()
            .parse()
            .expect("Invalid address format");
        socket
            .send_to(message.serialize().as_slice(), remote_addr)
            .unwrap();
    }
}
