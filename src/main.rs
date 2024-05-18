use rand::{distributions::Alphanumeric, Rng};
use std::collections::HashMap;
use std::fmt;
use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

// TODO: rewrite how the PeerBook is managed

// a probably poorly thought out substitute for fixed-size strings in messaging
type Str32 = [char; 32];

const EMPTY: Str32 = [' '; 32];

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

struct Heartbeat {
    sender: Peer,
    timestamp: u64,
    roster: Vec<Peer>,
}

struct Message {
    message_type: MessageTypes,
    payload: Box<[u8]>,
}

struct Peer {
    id: Str32,
    stream: Option<TcpStream>,
    incoming_address: Str32,
    listening_address: Str32,
}

impl Peer {
    // serialize just the incoming/listening addresses for messaging
    fn serialize(&self) -> std::io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(
            &self
                .incoming_address
                .iter()
                .map(|&c| c as u8)
                .collect::<Vec<u8>>(),
        );
        buffer.extend_from_slice(
            &self
                .listening_address
                .iter()
                .map(|&c| c as u8)
                .collect::<Vec<u8>>(),
        );

        Ok(buffer)
    }

    fn deserialize(reader: &mut impl Read) -> std::io::Result<Self> {
        let mut incoming_address = [0; 32];
        reader.read_exact(&mut incoming_address)?;
        let incoming_address: Str32 = incoming_address
            .iter()
            .map(|&c| c as char)
            .collect::<Vec<char>>()
            .try_into()
            .unwrap();

        let mut listening_address = [0; 32];
        reader.read_exact(&mut listening_address)?;
        let listening_address: Str32 = listening_address
            .iter()
            .map(|&c| c as char)
            .collect::<Vec<char>>()
            .try_into()
            .unwrap();

        Ok(Peer {
            id: EMPTY,
            stream: None,
            incoming_address,
            listening_address,
        })
    }
}

impl Clone for Peer {
    // NOTE: this leaves only the incoming/listening addresses
    fn clone(&self) -> Self {
        Peer {
            id: EMPTY,
            stream: None,
            incoming_address: self.incoming_address,
            listening_address: self.listening_address,
        }
    }
}

impl Debug for Peer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Peer {{ id: {:?}, incoming_address: {:?}, listening_address: {:?} }}",
            self.id.iter().collect::<String>().trim(),
            self.incoming_address.iter().collect::<String>().trim(),
            self.listening_address.iter().collect::<String>().trim()
        )
    }
}

impl Heartbeat {
    fn serialize(&self) -> std::io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&self.sender.serialize().unwrap());
        buffer.extend_from_slice(&self.timestamp.to_le_bytes());
        buffer.extend_from_slice(&(self.roster.len() as u32).to_le_bytes());
        for peer in self.roster.iter() {
            buffer.extend_from_slice(&peer.serialize().unwrap());
        }

        Ok(buffer)
    }

    fn deserialize(reader: &mut impl Read) -> std::io::Result<Self> {
        let sender = Peer::deserialize(reader)?;

        let mut timestamp = [0; 8];
        reader.read_exact(&mut timestamp)?;
        let timestamp = u64::from_le_bytes(timestamp);

        let mut roster_len = [0; 4];
        reader.read_exact(&mut roster_len)?;
        let roster_len = u32::from_le_bytes(roster_len) as usize;

        let mut roster = Vec::new();
        for _ in 0..roster_len {
            let peer = Peer::deserialize(reader)?;
            roster.push(peer);
        }

        Ok(Heartbeat {
            sender,
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

struct PeerBook {
    id_to_peer: HashMap<Str32, Peer>,
    listening_address_to_id: HashMap<Str32, Str32>,
    connecting_address_to_id: HashMap<Str32, Str32>,
}

impl PeerBook {
    fn new() -> Self {
        PeerBook {
            id_to_peer: HashMap::new(),
            listening_address_to_id: HashMap::new(),
            connecting_address_to_id: HashMap::new(),
        }
    }

    // NOTE: this only leaves the peers with their incoming/listening addresses
    fn roster(&self) -> Vec<Peer> {
        self.id_to_peer.values().map(|peer| peer.clone()).collect()
    }

    fn add_peer(&mut self, peer: Peer) {
        let new_peer = Peer {
            id: peer.id,
            stream: peer.stream,
            incoming_address: peer.incoming_address,
            listening_address: peer.listening_address,
        };

        println!("Adding peer: {:?}", new_peer.clone());

        self.id_to_peer.insert(peer.id, new_peer);
        self.listening_address_to_id
            .insert(peer.listening_address.clone(), peer.id);
        self.connecting_address_to_id
            .insert(peer.incoming_address.clone(), peer.id);
    }

    // check if this peer is in the peer book
    // and fill in missing fields
    fn upsert_peer(&mut self, peer: Peer) {
        // if the id is missing, add the peer
        if &peer.id != &EMPTY && !self.contains_id(&peer.id) {
            self.add_peer(peer);
        }
        // otherwise check if either the incoming or listening addresses are missing
        // and update if so
        else {
            let existing_peer = match self.listening_address_to_id.get(&peer.listening_address) {
                Some(id) => self.id_to_peer.get_mut(id).unwrap(),
                None => match self.connecting_address_to_id.get(&peer.incoming_address) {
                    Some(id) => self.id_to_peer.get_mut(id).unwrap(),
                    None => {
                        eprintln!("Peer not found: {:?}", peer);
                        return;
                    }
                },
            };

            if existing_peer.incoming_address == EMPTY {
                existing_peer.incoming_address = peer.incoming_address;
                self.connecting_address_to_id
                    .insert(peer.incoming_address.clone(), peer.id.clone());
            }

            if existing_peer.listening_address == EMPTY {
                existing_peer.listening_address = peer.listening_address;
                self.listening_address_to_id
                    .insert(peer.listening_address.clone(), peer.id.clone());
            }
        }
    }

    fn contains_id(&self, id: &Str32) -> bool {
        self.id_to_peer.contains_key(id)
    }

    fn remove_peer(&mut self, id: &Str32) {
        let peer = self.id_to_peer.get(id).unwrap();

        self.listening_address_to_id.remove(&peer.listening_address);
        self.connecting_address_to_id.remove(&peer.incoming_address);
        self.id_to_peer.remove(id);
    }

    fn len(&self) -> usize {
        self.id_to_peer.len()
    }

    fn iter_mut(&mut self) -> std::collections::hash_map::IterMut<Str32, Peer> {
        self.id_to_peer.iter_mut()
    }

    fn values_mut(&mut self) -> std::collections::hash_map::ValuesMut<Str32, Peer> {
        self.id_to_peer.values_mut()
    }

    fn lookup_mut(&mut self, peer: &Peer) -> Option<&mut Peer> {
        if peer.id != EMPTY {
            self.id_to_peer.get_mut(&peer.id)
        } else if peer.incoming_address != EMPTY {
            let id = self.connecting_address_to_id.get(&peer.incoming_address);
            self.id_to_peer.get_mut(id.unwrap())
        } else {
            let id = self.listening_address_to_id.get(&peer.listening_address);
            self.id_to_peer.get_mut(id.unwrap())
        }
    }
}

// 32 character random alphanumeric string
fn generate_id() -> Str32 {
    let id = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    stuff_string(id)
}

fn stuff_string(s: String) -> Str32 {
    let mut result = [' '; 32];
    for (i, c) in s.chars().enumerate().take(32) {
        result[i] = c;
    }

    result
}

fn make_connection(
    to_address: String,
    peer_book: &mut PeerBook,
    local_identities: &mut Vec<Str32>,
) {
    match TcpStream::connect(to_address.clone()) {
        Ok(stream) => {
            println!("Connected to {}", to_address);
            stream.set_nonblocking(true).unwrap();
            let peer = Peer {
                id: generate_id(),
                stream: Some(stream.try_clone().unwrap()),
                incoming_address: stuff_string(stream.peer_addr().unwrap().to_string()),
                listening_address: stuff_string(to_address.clone()),
            };

            peer_book.upsert_peer(peer);
            local_identities.push(stuff_string(stream.local_addr().unwrap().to_string()));

            println!(
                "Local identities: {:?}",
                local_identities.iter().collect::<Vec<&Str32>>()
            );
        }
        Err(e) => {
            eprintln!("Failed to connect to {}: {}", to_address, e);
        }
    };
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

    let listening_address = "127.0.0.1:".to_string() + port.trim();

    let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();

    let listener = TcpListener::bind(listening_address.clone()).expect("Could not bind");
    println!("Listening on {}", listening_address.clone());

    let connections = Arc::new(Mutex::new(PeerBook::new()));

    // list of outgoing ip:ports that correspond to this node
    // initialized with the node's listening port
    let local_identities = Arc::new(Mutex::new(vec![stuff_string(listening_address.clone())]));

    // spawn a new thread to listen for incoming connections
    // for each incoming connection, spawn a new thread to handle the connection
    // the handle_client function reads data from the connection and prints it
    thread::spawn({
        let connections = Arc::clone(&connections);
        move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let connecting_address: Str32 = stuff_string(
                            stream.peer_addr().unwrap().ip().to_string()
                                + ":"
                                + stream.peer_addr().unwrap().port().to_string().as_str(),
                        );

                        println!(
                            "New connection: {}",
                            connecting_address.clone().iter().collect::<String>()
                        );
                        stream.set_nonblocking(true).unwrap();

                        println!(
                            "ips in the stream: {:?}, {:?}",
                            stream.peer_addr().unwrap().ip().to_string()
                                + ":"
                                + stream.peer_addr().unwrap().port().to_string().as_str(),
                            stream.local_addr().unwrap().ip().to_string()
                                + ":"
                                + stream.local_addr().unwrap().port().to_string().as_str()
                        );

                        // they're connecting from an outgoing port,
                        // the listening address will be known from their heartbeats
                        let peer = Peer {
                            id: generate_id(),
                            stream: Some(stream.try_clone().unwrap()),
                            incoming_address: stuff_string(
                                connecting_address.iter().collect::<String>(),
                            ),
                            listening_address: EMPTY,
                        };

                        println!("peer: {:?}", peer);

                        let mut connections = match connections.lock() {
                            Ok(guard) => {
                                println!("locked successfully");
                                guard
                            }
                            Err(poisoned) => {
                                eprintln!("Mutex is poisoned: {:?}", poisoned);
                                return;
                            }
                        };
                        connections.add_peer(peer);
                        println!("Peer added!");
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
                    for peer in connections.values_mut() {
                        let connection = peer.stream.as_mut().unwrap();
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
        let connections_top = Arc::clone(&connections);
        let local_identities_top = Arc::clone(&local_identities);
        thread::spawn(move || loop {
            let mut connections = connections_top.lock().unwrap();
            let mut local_identities = local_identities_top.lock().unwrap();

            let mut new_roster = Vec::new();
            let mut dead_connections = Vec::new();

            for (id, peer) in connections.iter_mut() {
                let mut buffer = [0; 512];
                match peer.stream {
                    None => {
                        continue;
                    }
                    _ => {}
                }

                let connection = peer.stream.as_mut().unwrap();
                let outgoing_address = connection.peer_addr().unwrap();
                match connection.read(&mut buffer) {
                    Ok(0) => {
                        println!("Connection closed: {}", connection.peer_addr().unwrap());
                        dead_connections.push(id.clone());
                    }
                    Ok(n) => {
                        let message = Message::deserialize(&mut &buffer[..n]).unwrap();
                        match message.message_type {
                            MessageTypes::Heartbeat => {
                                let mut heartbeat =
                                    Heartbeat::deserialize(&mut &message.payload[..]).unwrap();

                                heartbeat.sender.incoming_address = stuff_string(
                                    outgoing_address.ip().to_string()
                                        + ":"
                                        + outgoing_address.port().to_string().as_str(),
                                );

                                println!(
                                    "Received heartbeat from {:?} at {}",
                                    heartbeat.sender, heartbeat.timestamp
                                );

                                println!(
                                    "heartbeat ips in the stream: {:?}, {:?}",
                                    connection.peer_addr().unwrap().ip().to_string()
                                        + ":"
                                        + connection
                                            .peer_addr()
                                            .unwrap()
                                            .port()
                                            .to_string()
                                            .as_str(),
                                    connection.local_addr().unwrap().ip().to_string()
                                        + ":"
                                        + connection
                                            .local_addr()
                                            .unwrap()
                                            .port()
                                            .to_string()
                                            .as_str()
                                );

                                //println!("Received roster: {:?}", heartbeat.roster);

                                new_roster.push(heartbeat.sender.clone());

                                // update connections with the roster
                                for peer in heartbeat.roster.iter() {
                                    new_roster.push(peer.clone());
                                }
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

            for id in dead_connections {
                connections.remove_peer(&id);
            }

            // TODO: Why doesn't this work?
            for peer in new_roster.iter() {
                if peer.listening_address != EMPTY
                    && !local_identities.contains(&peer.incoming_address)
                    && !local_identities.contains(&peer.listening_address)
                {
                    /*println!("Making connection to {:?}", peer);
                    make_connection(
                        peer.listening_address
                            .iter()
                            .collect::<String>()
                            .trim()
                            .to_string(),
                        &mut connections,
                        &mut local_identities,
                    );*/
                }
            }
        });
    }

    // make connection to `to_address`
    {
        let connections = Arc::clone(&connections);
        let local_identities = Arc::clone(&local_identities);
        make_connection(
            to_address.clone(),
            &mut connections.lock().unwrap(),
            &mut local_identities.lock().unwrap(),
        );
    }

    // heartbeats
    {
        let connections = Arc::clone(&connections);
        thread::spawn(move || loop {
            thread::sleep(std::time::Duration::from_secs(1));
            let mut connections = connections.lock().unwrap();
            let roster = connections.roster();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            println!(
                "current connections: {:?}",
                connections.iter_mut().collect::<Vec<(&Str32, &mut Peer)>>()
            );

            for peer in roster.clone() {
                let peer = connections.lookup_mut(&peer);
                let connection = peer.unwrap().stream.as_mut().unwrap();
                let heartbeat = Heartbeat {
                    sender: Peer {
                        id: EMPTY,
                        stream: None,
                        incoming_address: stuff_string(connection.peer_addr().unwrap().to_string()),
                        listening_address: stuff_string(listening_address.clone()),
                    },
                    timestamp,
                    roster: roster.clone(),
                };

                //println!("sending roster {:?}", roster.iter().collect::<Vec<&Peer>>());

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
