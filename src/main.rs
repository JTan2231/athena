use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

enum MessageTypes {
    Heartbeat = 1,
    UserMessage = 2,
}

impl MessageTypes {
    fn as_bytes(&self) -> &[u8] {
        match self {
            MessageTypes::Heartbeat => &[1],
            MessageTypes::UserMessage => &[2],
        }
    }
}

// TODO: still need to register the rosters on heartbeat acknowledgement
struct Heartbeat {
    sender_id: [char; 32],
    timestamp: u64,
    roster: Vec<[char; 32]>,
}

struct Message {
    message_type: MessageTypes,
    payload: Box<[u8]>,
}

impl Heartbeat {
    fn serialize(&self) -> std::io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&self.sender_id.iter().map(|&c| c as u8).collect::<Vec<u8>>());
        buffer.extend_from_slice(&self.timestamp.to_le_bytes());
        buffer.extend_from_slice(&(self.roster.len() as u32).to_le_bytes());
        for peer in self.roster.iter() {
            buffer.extend_from_slice(&peer.iter().map(|&c| c as u8).collect::<Vec<u8>>());
        }

        Ok(buffer)
    }

    fn deserialize(reader: &mut impl Read) -> std::io::Result<Self> {
        let mut sender_id = [0; 32];
        reader.read_exact(&mut sender_id)?;
        let sender_id: [char; 32] = sender_id
            .iter()
            .map(|&c| c as char)
            .collect::<Vec<char>>()
            .try_into()
            .unwrap();

        let mut timestamp = [0; 8];
        reader.read_exact(&mut timestamp)?;
        let timestamp = u64::from_le_bytes(timestamp);

        let mut roster_len = [0; 4];
        reader.read_exact(&mut roster_len)?;
        let roster_len = u32::from_le_bytes(roster_len) as usize;

        let mut roster = Vec::new();
        for _ in 0..roster_len {
            let mut peer = [0; 32];
            reader.read_exact(&mut peer)?;
            let peer: [char; 32] = peer
                .iter()
                .map(|&c| c as char)
                .collect::<Vec<char>>()
                .try_into()
                .unwrap();

            roster.push(peer);
        }

        Ok(Heartbeat {
            sender_id,
            timestamp,
            roster,
        })
    }
}

impl Message {
    fn serialize(&self) -> std::io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(self.message_type.as_bytes());
        buffer.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        buffer.extend_from_slice(&self.payload);

        Ok(buffer)
    }

    fn deserialize(reader: &mut impl Read) -> std::io::Result<Self> {
        let mut message_type = [0; 1];
        reader.read_exact(&mut message_type)?;
        let message_type = match message_type[0] {
            1 => MessageTypes::Heartbeat,
            2 => MessageTypes::UserMessage,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Message::deserialize: Invalid message type",
                ))
            }
        };

        let mut payload_len = [0; 4];
        reader.read_exact(&mut payload_len)?;
        let payload_len = u32::from_le_bytes(payload_len) as usize;

        let mut payload = vec![0; payload_len];
        reader.read_exact(&mut payload)?;

        Ok(Message {
            message_type,
            payload: payload.into_boxed_slice(),
        })
    }
}

fn stuff_string(s: String) -> [char; 32] {
    let mut result = [' '; 32];
    for (i, c) in s.chars().enumerate().take(32) {
        result[i] = c;
    }

    result
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <address> [--port <port>]", args[0]);
        std::process::exit(1);
    }

    let to_address = args[1].clone();

    let mut port = "8080";

    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" => {
                port = iter.next().unwrap();
            }
            _ => {}
        }
    }

    let address = "127.0.0.1:".to_string() + port.trim();

    let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();

    let listener = TcpListener::bind(address.clone()).expect("Could not bind");
    println!("Listening on {}", address.clone());

    let connections = Arc::new(Mutex::new(HashMap::new()));

    // spawn a new thread to listen for incoming connections
    // for each incoming connection, spawn a new thread to handle the connection
    // the handle_client function reads data from the connection and prints it
    thread::spawn({
        let connections = Arc::clone(&connections);
        move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let connecting_address: [char; 32] = stuff_string(
                            stream.peer_addr().unwrap().ip().to_string()
                                + ":"
                                + stream.peer_addr().unwrap().port().to_string().as_str(),
                        );

                        println!(
                            "New connection: {}",
                            connecting_address.clone().iter().collect::<String>()
                        );
                        stream.set_nonblocking(true).unwrap();
                        connections
                            .lock()
                            .unwrap()
                            .insert(connecting_address, stream.try_clone().unwrap());
                    }
                    Err(e) => {
                        eprintln!("Connection failed: {}", e);
                    }
                }
            }
        }
    });

    // spawn a new thread to send messages to all peers
    {
        let connections = Arc::clone(&connections);
        thread::spawn(move || loop {
            match rx.recv() {
                Ok(msg) => {
                    let mut connections = connections.lock().unwrap();
                    println!("connections size: {}", connections.len());
                    for connection in connections.values_mut() {
                        println!("Sending: {}", msg);
                        if let Err(e) = connection.write_all(msg.as_bytes()) {
                            eprintln!("Failed to send data: {}", e);
                        }
                    }
                    std::io::stdout().flush().unwrap();
                }
                Err(e) => {
                    eprintln!("Failed to receive message: {}", e);
                    break;
                }
            }
        });
    }

    // thread for reading messages from peers
    {
        let connections = Arc::clone(&connections);
        thread::spawn(move || loop {
            let mut connections = connections.lock().unwrap();
            let keys: Vec<[char; 32]> = connections.keys().cloned().collect();
            for id in keys.iter() {
                let mut buffer = [0; 512];
                let connection = connections.get_mut(id).unwrap();
                match connection.read(&mut buffer) {
                    Ok(0) => {
                        println!("Connection closed: {}", connection.peer_addr().unwrap());
                        connections.remove(id);
                    }
                    Ok(n) => {
                        let message = Message::deserialize(&mut &buffer[..n]).unwrap();
                        match message.message_type {
                            MessageTypes::Heartbeat => {
                                let heartbeat =
                                    Heartbeat::deserialize(&mut &message.payload[..]).unwrap();
                                println!(
                                    "Received heartbeat from {:?} at {}",
                                    id.iter().collect::<String>().trim(),
                                    heartbeat.timestamp
                                );
                            }
                            MessageTypes::UserMessage => {
                                println!(
                                    "Received message from {:?}: {}",
                                    id,
                                    String::from_utf8_lossy(&message.payload)
                                );
                            }
                        }
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(e) => {
                        eprintln!("Failed to read data: {}", e);
                    }
                }
            }
        });
    }

    // make connection to `to_address`
    {
        let connections = Arc::clone(&connections);
        match TcpStream::connect(to_address.clone()) {
            Ok(stream) => {
                println!("Connected to {}", to_address);
                // get to_address as a [char; 32]
                stream.set_nonblocking(true).unwrap();
                connections
                    .lock()
                    .unwrap()
                    .insert(stuff_string(to_address), stream.try_clone().unwrap());
            }
            Err(e) => {
                eprintln!("Failed to connect to {}: {}", to_address, e);
            }
        };
    }

    // heartbeats
    {
        let connections = Arc::clone(&connections);
        thread::spawn(move || loop {
            thread::sleep(std::time::Duration::from_secs(1));
            let roster: Vec<[char; 32]> = {
                let connections = connections.lock().unwrap();
                connections.keys().cloned().collect()
            };

            let mut connections = connections.lock().unwrap();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            for (_, connection) in connections.iter_mut() {
                let heartbeat = Heartbeat {
                    sender_id: stuff_string(address.clone()),
                    timestamp,
                    roster: roster.clone(),
                };

                let message = Message {
                    message_type: MessageTypes::Heartbeat,
                    payload: heartbeat.serialize().unwrap().into_boxed_slice(),
                };

                if let Err(e) = connection.write_all(&message.serialize().unwrap()) {
                    eprintln!("Failed to send heartbeat: {}", e);
                }
            }
        });
    }

    loop {
        // prompt user for text input to send
        // use > as a prompt
        let mut input = String::new();
        print!("> ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut input).unwrap();

        // send the input to all peers
        print!("Sending to tx: {}", input);
        if let Err(e) = tx.send(input.trim().to_string()) {
            eprintln!("Failed to send message: {}", e);
        }
    }
}
