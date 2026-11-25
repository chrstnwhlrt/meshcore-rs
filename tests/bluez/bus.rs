//! A D-Bus bus within the test: programs connect to it as to the system bus,
//! and there is one service on it.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener as Listener};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

/// The name of the service on the bus.
const SERVICE: &str = ":1.0";

/// The bus itself.
const DRIVER: &str = "org.freedesktop.DBus";

const CALL: u8 = 1;
const RETURN: u8 = 2;
const ERROR: u8 = 3;
const SIGNAL: u8 = 4;

/// What answers the calls of the programs.
pub trait Service: Clone + Send + 'static {
    /// A program calls a method.
    fn call(&self, call: Call);

    /// A program left the bus.
    fn left(&self, program: &str);
}

// ---------------------------------------------------------------- messages

/// The value of a property.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    I16(i16),
    Text(String),
    Path(String),
    Texts(Vec<String>),
    Bytes(Vec<u8>),
}

impl Value {
    const fn signature(&self) -> &'static str {
        match self {
            Self::Bool(_) => "b",
            Self::I16(_) => "n",
            Self::Text(_) => "s",
            Self::Path(_) => "o",
            Self::Texts(_) => "as",
            Self::Bytes(_) => "ay",
        }
    }

    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Self::Bool(value) => put_u32(out, u32::from(*value)),
            Self::I16(value) => {
                align(out, 2);
                out.extend(value.to_le_bytes());
            }
            Self::Text(text) | Self::Path(text) => put_text(out, text),
            Self::Texts(texts) => put_texts(out, texts),
            Self::Bytes(bytes) => put_array(out, 1, |out| out.extend(bytes)),
        }
    }
}

/// The properties of an interface.
pub type Properties = BTreeMap<String, Value>;

/// The interfaces of an object.
pub type Interfaces = BTreeMap<String, Properties>;

fn align(out: &mut Vec<u8>, to: usize) {
    out.resize(out.len().next_multiple_of(to), 0);
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    align(out, 4);
    out.extend(value.to_le_bytes());
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    put_u32(out, u32::try_from(text.len()).unwrap());
    out.extend(text.as_bytes());
    out.push(0);
}

fn put_signature(out: &mut Vec<u8>, signature: &str) {
    out.push(u8::try_from(signature.len()).unwrap());
    out.extend(signature.as_bytes());
    out.push(0);
}

fn put_array(out: &mut Vec<u8>, alignment: usize, put_items: impl FnOnce(&mut Vec<u8>)) {
    put_u32(out, 0);
    let len_at = out.len() - 4;
    align(out, alignment);
    let start = out.len();
    put_items(out);
    let len = u32::try_from(out.len() - start).unwrap();
    out[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
}

fn put_texts(out: &mut Vec<u8>, texts: &[String]) {
    put_array(out, 4, |out| {
        for text in texts {
            put_text(out, text);
        }
    });
}

fn put_variant(out: &mut Vec<u8>, value: &Value) {
    put_signature(out, value.signature());
    value.put(out);
}

fn put_properties(out: &mut Vec<u8>, properties: &Properties) {
    put_array(out, 8, |out| {
        for (name, value) in properties {
            align(out, 8);
            put_text(out, name);
            put_variant(out, value);
        }
    });
}

fn put_interfaces(out: &mut Vec<u8>, interfaces: &Interfaces) {
    put_array(out, 8, |out| {
        for (name, properties) in interfaces {
            align(out, 8);
            put_text(out, name);
            put_properties(out, properties);
        }
    });
}

/// The arguments of a message.
#[derive(Debug, Clone, Default)]
pub struct Body {
    signature: String,
    bytes: Vec<u8>,
}

impl Body {
    fn with(mut self, signature: &str, put: impl FnOnce(&mut Vec<u8>)) -> Self {
        self.signature.push_str(signature);
        put(&mut self.bytes);
        self
    }

    pub fn text(self, text: &str) -> Self {
        self.with("s", |out| put_text(out, text))
    }

    pub fn path(self, path: &str) -> Self {
        self.with("o", |out| put_text(out, path))
    }

    pub fn texts(self, texts: &[String]) -> Self {
        self.with("as", |out| put_texts(out, texts))
    }

    pub fn variant(self, value: &Value) -> Self {
        self.with("v", |out| put_variant(out, value))
    }

    pub fn properties(self, properties: &Properties) -> Self {
        self.with("a{sv}", |out| put_properties(out, properties))
    }

    pub fn interfaces(self, interfaces: &Interfaces) -> Self {
        self.with("a{sa{sv}}", |out| put_interfaces(out, interfaces))
    }

    pub fn objects(self, objects: &BTreeMap<String, Interfaces>) -> Self {
        self.with("a{oa{sa{sv}}}", |out| {
            put_array(out, 8, |out| {
                for (path, interfaces) in objects {
                    align(out, 8);
                    put_text(out, path);
                    put_interfaces(out, interfaces);
                }
            });
        })
    }

    /// Reads the arguments from the first one on.
    pub const fn reader(&self) -> Reader<'_> {
        Reader {
            data: self.bytes.as_slice(),
            at: 0,
        }
    }
}

/// Reads what a message holds.
pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn align(&mut self, to: usize) {
        self.at = self.at.next_multiple_of(to);
    }

    fn take(&mut self, len: usize) -> &'a [u8] {
        let taken = &self.data[self.at..self.at + len];
        self.at += len;
        taken
    }

    pub fn u32(&mut self) -> u32 {
        self.align(4);
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }

    /// A string or object path.
    pub fn text(&mut self) -> String {
        let len = self.u32() as usize;
        let text = String::from_utf8(self.take(len).to_vec()).unwrap();
        self.at += 1;
        text
    }

    /// An array of bytes.
    pub fn bytes(&mut self) -> Vec<u8> {
        let len = self.u32() as usize;
        self.take(len).to_vec()
    }

    fn signature(&mut self) -> String {
        let len = usize::from(self.take(1)[0]);
        let signature = String::from_utf8(self.take(len).to_vec()).unwrap();
        self.at += 1;
        signature
    }
}

/// A message as it is sent or received.
#[derive(Debug, Clone, Default)]
pub struct Message {
    kind: u8,
    serial: u32,
    /// The serial of the call this answers (0: none).
    answers: u32,
    pub path: String,
    pub interface: String,
    pub member: String,
    error: String,
    destination: String,
    sender: String,
    pub body: Body,
}

impl Message {
    fn encode(&self) -> Vec<u8> {
        let mut out = vec![b'l', self.kind, 0, 1];
        put_u32(&mut out, u32::try_from(self.body.bytes.len()).unwrap());
        put_u32(&mut out, self.serial);
        let texts = [
            (1, "o", &self.path),
            (2, "s", &self.interface),
            (3, "s", &self.member),
            (4, "s", &self.error),
            (6, "s", &self.destination),
            (7, "s", &self.sender),
        ];
        put_array(&mut out, 8, |out| {
            for (code, signature, text) in texts.into_iter().filter(|field| !field.2.is_empty()) {
                align(out, 8);
                out.push(code);
                put_signature(out, signature);
                put_text(out, text);
            }
            if self.answers != 0 {
                align(out, 8);
                out.push(5);
                put_signature(out, "u");
                put_u32(out, self.answers);
            }
            if !self.body.signature.is_empty() {
                align(out, 8);
                out.push(8);
                put_signature(out, "g");
                put_signature(out, &self.body.signature);
            }
        });
        align(&mut out, 8);
        out.extend(&self.body.bytes);
        out
    }

    async fn read(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Self> {
        let mut fixed = [0; 16];
        stream.read_exact(&mut fixed).await?;
        assert_eq!(fixed[0], b'l', "a message that is not little endian");
        let number = |at: usize| u32::from_le_bytes(fixed[at..at + 4].try_into().unwrap());
        let fields_len = number(12) as usize;
        let mut fields = vec![0; fields_len.next_multiple_of(8)];
        stream.read_exact(&mut fields).await?;
        let mut bytes = vec![0; number(4) as usize];
        stream.read_exact(&mut bytes).await?;

        let mut message = Self {
            kind: fixed[1],
            serial: number(8),
            ..Self::default()
        };
        let mut reader = Reader {
            data: &fields[..fields_len],
            at: 0,
        };
        while reader.at < fields_len {
            reader.align(8);
            let code = reader.take(1)[0];
            let signature = reader.signature();
            let field = match code {
                1 => &mut message.path,
                2 => &mut message.interface,
                3 => &mut message.member,
                4 => &mut message.error,
                5 => {
                    message.answers = reader.u32();
                    continue;
                }
                6 => &mut message.destination,
                7 => &mut message.sender,
                8 => {
                    message.body.signature = reader.signature();
                    continue;
                }
                _ => panic!("a message with the header field {code} of type {signature}"),
            };
            *field = reader.text();
        }
        message.body.bytes = bytes;
        Ok(message)
    }
}

// --------------------------------------------------------------- the bus

/// A program on the bus.
#[derive(Clone)]
pub struct Program {
    pub name: String,
    out: mpsc::UnboundedSender<Message>,
    serial: Arc<AtomicU32>,
    /// The calls it has not answered yet.
    calls: Arc<Mutex<HashMap<u32, oneshot::Sender<Message>>>>,
}

impl Program {
    fn send(&self, message: Message) -> u32 {
        let serial = self.serial.fetch_add(1, Ordering::SeqCst);
        let _ = self.out.send(Message { serial, ..message });
        serial
    }

    /// Calls a method of the program; returns what it answers, or `None` when
    /// it answers with an error or leaves.
    pub async fn call(
        &self,
        path: &str,
        interface: &str,
        member: &str,
        body: Body,
    ) -> Option<Body> {
        let (answered, answer) = oneshot::channel();
        {
            let mut calls = self.calls.lock().unwrap();
            let serial = self.send(Message {
                kind: CALL,
                path: path.into(),
                interface: interface.into(),
                member: member.into(),
                destination: self.name.clone(),
                sender: SERVICE.into(),
                body,
                ..Message::default()
            });
            calls.insert(serial, answered);
        }
        let answer = answer.await.ok()?;
        (answer.kind == RETURN).then_some(answer.body)
    }
}

/// A call of a method.
pub struct Call {
    pub program: Program,
    pub message: Message,
}

impl Call {
    fn answer(&self, kind: u8, error: &str, sender: &str, body: Body) {
        self.program.send(Message {
            kind,
            answers: self.message.serial,
            error: error.into(),
            destination: self.program.name.clone(),
            sender: sender.into(),
            body,
            ..Message::default()
        });
    }

    /// Answers the call.
    pub fn reply(&self, body: Body) {
        self.answer(RETURN, "", SERVICE, body);
    }

    /// Answers with an error.
    pub fn fail(&self, error: &str, text: &str) {
        self.answer(ERROR, error, SERVICE, Body::default().text(text));
    }

    /// Answers a call of the bus itself.
    fn driver(&self) {
        let body = match self.message.member.as_str() {
            "Hello" => Body::default().text(&self.program.name),
            "AddMatch" | "RemoveMatch" => Body::default(),
            member => panic!("{DRIVER} is asked for {member}"),
        };
        self.answer(RETURN, "", DRIVER, body);
    }
}

/// The programs on the bus.
#[derive(Clone, Default)]
pub struct Bus(Arc<Mutex<Vec<Program>>>);

impl Bus {
    /// Sends a signal to all programs.
    pub fn signal(&self, path: &str, interface: &str, member: &str, body: &Body) {
        for program in &*self.0.lock().unwrap() {
            program.send(Message {
                kind: SIGNAL,
                path: path.into(),
                interface: interface.into(),
                member: member.into(),
                sender: SERVICE.into(),
                body: body.clone(),
                ..Message::default()
            });
        }
    }

    /// Listens on an abstract socket.
    pub fn listen(socket: &str) -> Listener {
        let address = SocketAddr::from_abstract_name(socket).unwrap();
        let listener = Listener::bind_addr(&address).unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    }

    /// Takes in the programs that connect to the socket.
    pub async fn serve(self, listener: Listener, service: impl Service) {
        let listener = UnixListener::from_std(listener).unwrap();
        for number in 1..=u32::MAX {
            let (stream, _) = listener.accept().await.unwrap();
            let (bus, service) = (self.clone(), service.clone());
            tokio::spawn(async move {
                // It ends when the program leaves.
                let _ = bus
                    .serve_program(format!(":1.{number}"), stream, service)
                    .await;
            });
        }
    }

    async fn serve_program(
        self,
        name: String,
        stream: UnixStream,
        service: impl Service,
    ) -> io::Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        // the authentication, which takes whoever it is
        reader.read_exact(&mut [0]).await?;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let answer = match line.trim_end() {
                "BEGIN" => break,
                line if line.starts_with("AUTH EXTERNAL ") => {
                    "OK 00112233445566778899aabbccddeeff\r\n"
                }
                _ => "ERROR\r\n",
            };
            writer.write_all(answer.as_bytes()).await?;
        }

        let (out, mut messages) = mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(message) = messages.recv().await {
                if writer.write_all(&message.encode()).await.is_err() {
                    break;
                }
            }
        });
        let program = Program {
            name,
            out,
            serial: Arc::new(AtomicU32::new(1)),
            calls: Arc::default(),
        };
        self.0.lock().unwrap().push(program.clone());
        while let Ok(message) = Message::read(&mut reader).await {
            if message.kind != CALL {
                let call = program.calls.lock().unwrap().remove(&message.answers);
                if let Some(call) = call {
                    let _ = call.send(message);
                }
                continue;
            }
            let call = Call {
                program: program.clone(),
                message,
            };
            if call.message.destination == DRIVER {
                call.driver();
            } else {
                service.call(call);
            }
        }
        self.0.lock().unwrap().retain(|p| p.name != program.name);
        program.calls.lock().unwrap().clear();
        service.left(&program.name);
        Ok(())
    }
}
