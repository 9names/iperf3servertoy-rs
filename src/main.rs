use std::sync::{Arc, Mutex};

use async_std::io::{self, prelude::*};
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::task;
use once_cell::sync::Lazy;

// These are used as system state and as messages in original iperf.
// Just using them for messages for now.
#[allow(dead_code)]
mod iperf_command {
    pub const TEST_START: u8 = 1;
    pub const TEST_RUNNING: u8 = 2;
    pub const TEST_END: u8 = 4;
    pub const PARAM_EXCHANGE: u8 = 9;
    pub const CREATE_STREAMS: u8 = 10;
    pub const SERVER_TERMINATE: u8 = 11;
    pub const CLIENT_TERMINATE: u8 = 12;
    pub const EXCHANGE_RESULTS: u8 = 13;
    pub const DISPLAY_RESULTS: u8 = 14;
    pub const IPERF_START: u8 = 15;
    pub const IPERF_DONE: u8 = 16;
}
static SESSIONS: Lazy<Arc<Mutex<Vec<[u8; 36]>>>> = Lazy::new(|| Arc::new(Mutex::new(vec![])));

fn main() -> io::Result<()> {
    task::block_on(async {
        // All connections will come in on port 5201 by default
        // switch this to 0.0.0.0:5201 to be able to connect to this server from another machine
        let listener = TcpListener::bind("127.0.0.1:5201").await?;
        println!("Listening on {}", listener.local_addr()?);

        let mut incoming = listener.incoming();

        while let Some(stream) = incoming.next().await {
            let stream = stream?;
            task::spawn(async {
                process(stream).await.unwrap();
            });
        }
        Ok(())
    })
}

async fn process(stream: TcpStream) -> io::Result<()> {
    println!("Accepted from: {}", stream.peer_addr()?);
    let mut buf_reader = stream.clone();
    let mut buf_writer = stream;
    // The first thing an iperf3 client will send is
    // a 36 character, nul terminated session id cookie
    let mut session_id_cstr: [u8; 37] = [0; 37];
    let mut session_id: [u8; 36] = [0; 36];
    buf_reader.read_exact(&mut session_id_cstr).await?;
    // The string is a C style nul terminated thing, so strip that off.
    session_id.copy_from_slice(&session_id_cstr[..36]);

    // If this is the first we've seen this session cookie, assume we're the control channel
    let control_channel = if session_id_cstr.is_ascii() {
        println!("cookie: {}", core::str::from_utf8(&session_id).unwrap());
        let sess = SESSIONS.clone();
        let existing_session = sess.lock().unwrap().contains(&session_id);
        if !existing_session {
            sess.lock().unwrap().push(session_id);
            true
        } else {
            false
        }
    } else {
        // Not an IO error, just an unhandled data stream.
        return Ok(());
    };

    if control_channel {
        println!("control channel opened");
        // need to disable Nagle algorithm to ensure commands are sent promptly
        buf_writer.set_nodelay(true).unwrap();

        println!("ask the client to send the config parameters (we won't read them)");
        buf_writer
            .write_all(&[iperf_command::PARAM_EXCHANGE])
            .await?;

        let mut message_len: [u8; 4] = [0; 4];
        buf_reader.read_exact(&mut message_len).await?;
        let message_len = u32::from_be_bytes(message_len);
        println!("Config JSON length {}", message_len);

        let mut buf: Vec<u8> = vec![0u8; message_len as usize];
        buf_reader.read_exact(&mut buf).await?;
        if buf.is_ascii() {
            let string = String::from_utf8(buf.to_vec()).unwrap();
            println!("config: {}", string);
        }

        println!("ask the client to connect to a 2nd socket");
        buf_writer
            .write_all(&[iperf_command::CREATE_STREAMS])
            .await?;

        println!("ask the client to start the test");
        buf_writer.write_all(&[iperf_command::TEST_START]).await?;

        // should probably wait for some data on the other channel for this
        println!("tell the client we've started running the test");
        buf_writer.write_all(&[iperf_command::TEST_RUNNING]).await?;

        // the client should eventually reply with a command
        let mut reply: [u8; 1] = [0; 1];
        buf_reader.read_exact(&mut reply).await?;

        // We're hoping that it was TEST_END. check:
        if reply[0] == iperf_command::TEST_END {
            println!("TEST_END command received from client");
            // can't exchange results, we haven't calculated them.
            // buf_writer.write_all(&[iperf_command::EXCHANGE_RESULTS]);

            buf_writer
                .write_all(&[iperf_command::DISPLAY_RESULTS])
                .await?;
            let sess = SESSIONS.clone();

            println!("dropping session cookie now");
            sess.lock().unwrap().retain(|&f| f != session_id);
        } else {
            println!("were expecting TEST_END, got {}", reply[0]);
        }

        // Should be done now, check:
        let mut reply: [u8; 1] = [0; 1];
        buf_reader.read_exact(&mut reply).await?;
        if reply[0] == iperf_command::IPERF_DONE {
            println!("Client says we're good, ship it!");
        } else {
            println!("were expecting IPERF_DONE, got {}", reply[0]);
        }

        // Collect any data remaining in the channel, it's about to close
        let mut buf: Vec<u8> = vec![];
        let _ = buf_reader.read_to_end(&mut buf).await;
        if !buf.is_empty() {
            println!("Printing out any remaining data in control channel...");
            if buf.is_ascii() {
                let string = String::from_utf8(buf).unwrap();
                println!("buf: {}", string);
            } else {
                println!("buf: {:?}", buf);
            }
        }
        println!("control channel is done");
    } else {
        // this will be the second connection from the client
        println!("data channel opened");
        let mut message: [u8; 0x1000] = [0; 0x1000];
        let mut bytes_total: u64 = 0;
        let mut done = false;
        while !done {
            let sz = buf_reader.read(&mut message).await?;
            if sz == 0 {
                done = true;
            }
            bytes_total += sz as u64;
        }
        println!("we're done receiving. received {} bytes", bytes_total);
    }
    Ok(())
}
