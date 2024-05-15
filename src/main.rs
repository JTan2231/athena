use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

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

    let connections = Arc::new(Mutex::new(Vec::new()));

    // spawn a new thread to listen for incoming connections
    // for each incoming connection, spawn a new thread to handle the connection
    // the handle_client function reads data from the connection and prints it
    thread::spawn({
        let connections = Arc::clone(&connections);
        move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        println!("New connection: {}", stream.peer_addr().unwrap());
                        stream.set_nonblocking(true).unwrap();
                        connections
                            .lock()
                            .unwrap()
                            .push(stream.try_clone().unwrap());
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
        thread::spawn(move || {
            loop {
                match rx.recv() {
                    Ok(msg) => {
                        let mut connections = connections.lock().unwrap();
                        println!("connections size: {}", connections.len());
                        for connection in connections.iter_mut() {
                            println!("Sending: {}", msg);
                            if let Err(e) = connection.write_all(msg.as_bytes()) {
                                eprintln!("Failed to send data: {}", e);
                            }
                        }
                        std::io::stdout().flush().unwrap(); // Flush stdout
                    }
                    Err(e) => {
                        eprintln!("Failed to receive message: {}", e);
                        break; // Exit loop if there's an error
                    }
                }
            }
        });
    }

    // thread for reading messages from peers
    {
        let connections = Arc::clone(&connections);
        thread::spawn(move || loop {
            for connection in connections.lock().unwrap().iter_mut() {
                let mut buffer = [0; 512];
                match connection.read(&mut buffer) {
                    Ok(0) => {
                        println!("Connection closed: {}", connection.peer_addr().unwrap());
                    }
                    Ok(n) => {
                        println!("Received: {}", String::from_utf8_lossy(&buffer[..n]));
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
                stream.set_nonblocking(true).unwrap();
                connections
                    .lock()
                    .unwrap()
                    .push(stream.try_clone().unwrap());
            }
            Err(e) => {
                eprintln!("Failed to connect to {}: {}", to_address, e);
            }
        };
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
