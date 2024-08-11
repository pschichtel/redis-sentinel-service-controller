use std::{
    env,
    fmt::Display,
    net::SocketAddr,
    process::ExitCode,
    sync::{
        mpsc::{self, Sender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use redis::{cmd, Client, Cmd, Connection, ControlFlow, PubSubCommands, RedisError};

use std::net::ToSocketAddrs;

fn get_master_from_sentinel_cmd(name: &str) -> Cmd {
    let mut cmd = cmd("SENTINEL");
    cmd.arg("get-master-addr-by-name").arg(name);
    return cmd;
}

#[derive(Debug)]
enum Error {
    RedisErr(RedisError),
    InvalidResponse(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::RedisErr(err) => write!(f, "RedisError({})", err),
            Error::InvalidResponse(err) => write!(f, "InvalidResponse({})", err),
        }
    }
}

type RedisAddr = (String, u16);

fn get_master_from_sentinel(
    connection: &mut Connection,
    master_name: &str,
) -> Result<RedisAddr, Error> {
    let response = match get_master_from_sentinel_cmd(master_name).query::<Vec<String>>(connection)
    {
        Ok(response) => response,
        Err(redis_err) => return Err(Error::RedisErr(redis_err)),
    };

    if response.len() != 2 {
        return Err(Error::InvalidResponse(
            "Response did not have exactly 2 elements!".to_owned(),
        ));
    }

    let host: String = response[0].to_owned();
    let port: u16 = match response[1].parse() {
        Ok(p) => p,
        Err(err) => return Err(Error::InvalidResponse(format!("Port is invalid: {}", err))),
    };

    return Ok((host, port));
}

fn listen_for_master_switches(
    client: Arc<Client>,
    sender: Sender<RedisAddr>,
    master_name: &str,
) -> JoinHandle<()> {
    let master_name = master_name.to_string();
    return thread::spawn(move || loop {
        let mut connection = match client.get_connection() {
            Ok(c) => c,
            Err(err) => {
                eprintln!("Failed to connect: {}", err);
                continue;
            }
        };
        let topic = "+switch-master";
        let subscribe_result = connection.subscribe::<_, _, ()>(topic, |msg| {
            let value: String = msg.get_payload().unwrap();
            let segments: Vec<&str> = value
                .as_str()
                .split_ascii_whitespace()
                .into_iter()
                .collect();
            if segments.len() < 5 {
                eprintln!("Received invalid switch-master event: {:?}", segments);
                return ControlFlow::Continue;
            }
            let affected_master = segments[0];
            if master_name.as_str() != affected_master {
                println!(
                    "Master changed for {}, we are not interested in that...",
                    affected_master
                );
                return ControlFlow::Continue;
            }
            let host = segments[3].to_owned();
            let port: u16 = segments[4].parse().unwrap();
            sender.send((host, port)).unwrap();
            ControlFlow::Continue
        });

        if let Err(err) = subscribe_result {
            eprintln!("Failed to subscribe to topic {}: {}", topic, err);
            continue;
        }
    });
}

fn poll_master_address(
    client: Arc<Client>,
    sender: Sender<RedisAddr>,
    master_name: &str,
    poll_interval: &Duration,
) -> JoinHandle<()> {
    let master_name = master_name.to_string();
    let poll_interval = *poll_interval;
    return thread::spawn(move || loop {
        let mut connection = match client.get_connection() {
            Ok(c) => c,
            Err(err) => {
                eprintln!("Failed to connect: {}", err);
                continue;
            }
        };
        match get_master_from_sentinel(&mut connection, master_name.as_str()) {
            Ok(master) => {
                sender.send(master).unwrap();
            }
            Err(err) => {
                eprintln!("Failed to get initial master: {}", err);
            }
        };
        thread::sleep(poll_interval);
    });
}

fn materialize_service(addr: &RedisAddr) {
    let socket_addrs: Vec<SocketAddr> = match addr.to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(err) => {
            eprintln!("Failed to resolve the address: {}", err);
            return;
        }
    };

    for addr in socket_addrs {
        println!("Resolved: {}", addr);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        eprintln!("Wrong arguments!");
        eprintln!(
            "Usage: {} <sentinal host:port> <master name> <poll interval secs>",
            args[0]
        );
        return ExitCode::FAILURE;
    }
    let sentinel_addr = args[1].clone();
    let master_name = args[2].clone();
    let poll_interval = Duration::from_secs(args[3].parse().unwrap());
    let client = Arc::new(redis::Client::open(format!("redis://{}/", sentinel_addr)).unwrap());
    let mut connection = client.get_connection().unwrap();
    let initial_master = match get_master_from_sentinel(&mut connection, master_name.as_str()) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("Failed to get initial master: {}", err);
            return ExitCode::FAILURE;
        }
    };

    println!("Master: {:?}", initial_master);
    materialize_service(&initial_master);

    let (tx, rx) = mpsc::channel::<RedisAddr>();

    let _ = listen_for_master_switches(client.clone(), tx.clone(), master_name.as_str());
    let _ = poll_master_address(
        client.clone(),
        tx.clone(),
        master_name.as_str(),
        &poll_interval,
    );

    loop {
        let addr = match rx.recv() {
            Ok(addr) => addr,
            Err(err) => {
                eprintln!("Failed to receive: {}", err);
                continue;
            }
        };

        println!("Received new master: {:?}", addr);
        materialize_service(&addr);
    }
}
