use anyhow::Result;
use std::{sync::Arc, vec};
use tokio::io::AsyncBufReadExt;
use tokio::runtime::Runtime;
use tokio::{
    io::Interest,
    net::{TcpListener, TcpStream, ToSocketAddrs},
    select,
    sync::RwLock,
    sync::Semaphore,
};

type SharedString = Arc<RwLock<String>>;

fn main() -> Result<()> {
    let runtime = Runtime::new().expect("failed to start new Runtime");
    let (stream, server) = runtime.block_on(create_stream("127.0.0.1:5454"))?;
    let state = Arc::new(RwLock::new(String::from("state is not initialized")));

    let close_handler = Arc::new(Semaphore::new(0));
    if let Some(server) = server {
        runtime.spawn(run_server(
            Arc::new(server),
            state.clone(),
            close_handler.clone(),
        ));
    }

    if let Some(stream) = stream {
        runtime.spawn(handle_stream(
            Arc::new(stream),
            state.clone(),
            close_handler.clone(),
        ));
    }

    let state1 = state.clone();
    let close_handler_1 = close_handler.clone();
    runtime.spawn(async move {
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
        loop {
            let mut line = String::new();
            select! {
                _ = stdin.read_line(&mut line) => {
                    let mut state = state1.write().await;
                    *state = line;
                }
                _ = close_handler_1.acquire() => {
                    break;
                }
            }
        }
    });

    let state2 = state;
    let close_handler_2 = close_handler.clone();
    runtime.spawn(async move {
        loop {
            select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                    let state = state2.read().await;
                    println!(
                        "{}{}{state}",
                        termion::clear::All,
                        termion::cursor::Goto(1, 1)
                    );
                }
                _ = close_handler_2.acquire() => {
                    break;
                }
            }
        }
    });

    runtime.block_on(tokio::signal::ctrl_c())?;
    close_handler.close();
    runtime.shutdown_timeout(tokio::time::Duration::from_secs(1));

    Ok(())
}

async fn create_stream(
    addr: impl ToSocketAddrs,
) -> Result<(Option<TcpStream>, Option<TcpListener>)> {
    let stream = TcpStream::connect(&addr).await;
    if let Ok(stream) = stream {
        println!("Connected to {}", stream.local_addr()?);
        return Ok((Some(stream), None));
    }
    let e = stream.unwrap_err();

    if e.kind() != std::io::ErrorKind::ConnectionRefused {
        return Err(e.into());
    }

    println!("No server found, starting one");
    // create connection
    let server = TcpListener::bind(addr).await?;
    Ok((None, Some(server)))
}

async fn run_server(
    server: Arc<TcpListener>,
    state: SharedString,
    close_handler: Arc<Semaphore>,
) -> Result<()> {
    loop {
        select! {
            connection = server.accept() => {
                let (socket, addr)= connection?;
                println!("[SERVER] New connection from {addr}");
                tokio::spawn(handle_stream(Arc::new(socket), state.clone(), close_handler.clone()));

            }
            _ = close_handler.acquire() => {
                println!("Server close signal received, server will not accept new connections.");
                break;
            }
        }
    }
    Ok(())
}

async fn handle_stream(
    stream: Arc<TcpStream>,
    state: SharedString,
    close_handler: Arc<Semaphore>,
) -> Result<()> {
    let mut last_state = String::new();
    loop {
        select! {
            ready = stream.ready(Interest::READABLE | Interest::WRITABLE) => {
                let ready = ready?;
                if ready.is_readable() {
                    read_stream(stream.clone(), state.clone()).await?;
                }
                if ready.is_writable() {
                    let state_str = state.read().await;
                    if *state_str != last_state {
                        write_stream(stream.clone(), state.clone()).await?;
                        last_state = state_str.clone();
                    }
                }
            }
            _ = close_handler.acquire() => {
                println!("[STREAM] {} closed", stream.local_addr()?);
                break;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    Ok(())
}

async fn read_stream(stream: Arc<TcpStream>, state: SharedString) -> Result<()> {
    let mut data = vec![0; 1024];
    match stream.try_read(&mut data) {
        Ok(n) => {
            //println!("[STREAM] {} read {} bytes", stream.local_addr()?, n);
            let size = data[0] as usize;
            let data = &data[1..size + 1];
            let data = String::from_utf8_lossy(data);
            *state.write().await = data.to_string();

            Ok(())
        }
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(e.into()),
    }
}

async fn write_stream(stream: Arc<TcpStream>, state: SharedString) -> Result<()> {
    let mut data = vec![];
    let payload = state.read().await;
    let payload = (*payload).bytes();
    data.push(payload.len() as u8);
    data.extend(payload);

    match stream.try_write(&data) {
        Ok(n) => {
            // println!("wrote {} bytes", n);
            Ok(())
        }
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(e.into()),
    }
}
