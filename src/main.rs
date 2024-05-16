mod frame;

extern crate mio;

use crate::frame::WebSocketFrame;
use http_muncher::{Parser, ParserHandler};
use mio::{tcp::*, EventLoop, EventSet, Handler, PollOpt, Token, TryRead, TryWrite};
use rustc_serialize::base64::{ToBase64, STANDARD};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::rc::Rc;

fn gen_key(key: &String) -> String {
    let mut m = sha1::Sha1::new();
    let mut buf = [0u8; 20];

    m.update(key.as_bytes());
    m.update("258EAFA5-E914-47DA-95CA-C5AB0DC85B11".as_bytes());

    m.output(&mut buf);

    return buf.to_base64(STANDARD);
}

struct HttpParser {
    current_key: Option<String>,
    headers: Rc<RefCell<HashMap<String, String>>>,
}

impl ParserHandler for HttpParser {
    fn on_header_field(&mut self, s: &[u8]) -> bool {
        self.current_key = Some(std::str::from_utf8(s).unwrap().to_string());
        true
    }

    fn on_header_value(&mut self, s: &[u8]) -> bool {
        self.headers.borrow_mut().insert(
            self.current_key.clone().unwrap(),
            std::str::from_utf8(s).unwrap().to_string(),
        );
        true
    }

    fn on_headers_complete(&mut self) -> bool {
        false
    }
}

enum ClientState {
    AwaitingHandshake(RefCell<Parser<HttpParser>>),
    // AwaitingHandshake,
    HandshakeResponse,
    Connected,
}

// Client WS
struct WebSocketClient {
    socket: TcpStream,
    // http_parser: Parser<HttpParser>,
    headers: Rc<RefCell<HashMap<String, String>>>,
    interest: EventSet,
    state: ClientState,
}

impl WebSocketClient {
    fn write(&mut self) {
        let headers = self.headers.borrow();
        // Find the header that interests us, and generate the key from its value:
        let response_key = gen_key(&headers.get("Sec-WebSocket-Key").unwrap());
        let response = fmt::format(format_args!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {response_key}\r\nconnect-src: '*'\r\nUpgrade: websocket\r\n\r\n"));

        self.socket.try_write(response.as_bytes()).unwrap();

        self.state = ClientState::Connected;

        self.interest.remove(EventSet::writable());
        self.interest.insert(EventSet::readable());
    }

    fn read(&mut self) {
        match self.state {
            ClientState::AwaitingHandshake(_) => {
                self.read_handshake();
            }
            ClientState::Connected => {
                let frame = WebSocketFrame::read(&mut self.socket);
                match frame {
                    Ok(frame) => println!("{:?}", frame),
                    Err(e) => println!("error while reading frame: {e}"),
                }
            }
            _ => {}
        }
    }

    fn read_handshake(&mut self) {
        loop {
            let mut buf = [0; 2048];
            match self.socket.try_read(&mut buf) {
                Err(e) => {
                    println!("Error while reading socket: {:?}", e);
                    return;
                }
                Ok(None) =>
                // Socket buffer has got no more bytes.println!("qqqqq");
                {
                    break
                }
                Ok(Some(_len)) => {
                    let is_upgrade =
                        if let ClientState::AwaitingHandshake(ref parser_state) = self.state {
                            let mut parser = parser_state.borrow_mut();
                            parser.parse(&buf);
                            parser.is_upgrade()
                        } else {
                            false
                        };
                    println!("Is Upgrade: {is_upgrade}");

                    if is_upgrade {
                        self.state = ClientState::Connected;
                        self.interest.remove(EventSet::readable());

                        self.interest.insert(EventSet::writable());
                        break;
                    }
                    // self.http_parser.parse(&buf);
                    // if self.http_parser.is_upgrade() {
                    //     // Change the current state
                    //     self.state = ClientState::Connected;

                    //     // Change current interest to `Writable`
                    //     self.interest.remove(EventSet::readable());
                    //     self.interest.insert(EventSet::writable());
                    //     break;
                    // }
                }
            }
        }
    }

    fn new(socket: TcpStream) -> WebSocketClient {
        let headers = Rc::new(RefCell::new(HashMap::new()));
        WebSocketClient {
            socket,
            headers: headers.clone(),
            // http_parser: Parser::request(HttpParser {
            //     current_key: None,
            //     headers: headers.clone(),
            // }),
            interest: EventSet::readable(),
            // state: ClientState::AwaitingHandshake,
            state: ClientState::AwaitingHandshake(RefCell::new(Parser::request(HttpParser {
                current_key: None,
                headers: headers.clone(),
            }))),
        }
    }
}

// Server WS
struct WebSocketServer {
    socket: TcpListener,
    clients: HashMap<Token, WebSocketClient>,
    token_counter: usize,
}

const SERVER_TOKEN: Token = Token(0);

impl Handler for WebSocketServer {
    type Timeout = usize;
    type Message = ();

    fn ready(
        &mut self,
        event_loop: &mut EventLoop<WebSocketServer>,
        token: Token,
        event: EventSet,
    ) {
        if event.is_readable() {
            match token {
                SERVER_TOKEN => {
                    let client_socket = match self.socket.accept() {
                        Err(e) => {
                            println!("Accept Error: {}", e);
                            return;
                        }
                        Ok(None) => unreachable!("Accept has returned 'None'"),
                        Ok(Some((sock, _addr))) => sock,
                    };

                    self.token_counter += 1;
                    let new_token = Token(self.token_counter);

                    self.clients
                        .insert(new_token, WebSocketClient::new(client_socket));
                    event_loop
                        .register(
                            &self.clients[&new_token].socket,
                            new_token,
                            EventSet::readable(),
                            PollOpt::edge() | PollOpt::oneshot(),
                        )
                        .unwrap();
                }
                token => {
                    // let mut client_socket: String;
                    let client = self.clients.get_mut(&token).unwrap();
                    client.read();
                    event_loop
                        .reregister(
                            &client.socket,
                            token,
                            client.interest,
                            PollOpt::edge() | PollOpt::oneshot(),
                        )
                        .unwrap();
                }
            }
        }

        if event.is_writable() {
            let client = self.clients.get_mut(&token).unwrap();
            client.write();
            event_loop
                .reregister(
                    &client.socket,
                    token,
                    client.interest,
                    PollOpt::edge() | PollOpt::oneshot(),
                )
                .unwrap()
        }
    }
}

fn main() {
    let address = "0.0.0.0:5000".parse::<SocketAddr>().unwrap();
    let server_socket = TcpListener::bind(&address).unwrap();

    let mut event_loop = EventLoop::new().unwrap();

    let mut server = WebSocketServer {
        socket: server_socket,
        clients: HashMap::new(),
        token_counter: 1,
    };

    event_loop
        .register(
            &server.socket,
            Token(0),
            EventSet::readable(),
            PollOpt::edge(),
        )
        .unwrap();

    event_loop.run(&mut server).unwrap();
}
