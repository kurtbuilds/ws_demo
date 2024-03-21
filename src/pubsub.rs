use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use futures::stream::SplitSink;
use sid::Sid;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::mpsc::error::SendError;
use tokio::task::JoinHandle;

pub enum Message {
    Register(UserId, WebSocket, SocketAddr),
    Unregister(UserId, SocketAddr),
    Message(UserId, String),
}

pub type UserId = &'static str;


pub struct PubSub<T>(Arc<UnboundedSender<T>>);

impl<T> Clone for PubSub<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl PubSub<Message> {
    pub fn register(&self, user_id: UserId, ws: WebSocket, who: SocketAddr) -> Result<(), SendError<Message>> {
        self.0.send(Message::Register(user_id, ws, who))
    }

    pub fn unsubscribe(&self, user_id: UserId, who: SocketAddr) -> Result<(), SendError<Message>> {
        self.0.send(Message::Unregister(user_id, who))
    }

    pub fn send_message(&self, user_id: UserId, message: String) -> Result<(), SendError<Message>> {
        self.0.send(Message::Message(user_id, message))
    }
}

pub type SendSocket = SplitSink<WebSocket, axum::extract::ws::Message>;

pub struct SocketClient {
    who: SocketAddr,
    tx: SendSocket,
    listener: JoinHandle<()>,
}

pub struct PubSubActor {
    id: Sid,
    pubsub: PubSub<Message>,
    register: HashMap<UserId, Vec<SocketClient>>,
    rx: UnboundedReceiver<Message>,
}

impl PubSubActor {
    pub fn pubsub(&self) -> PubSub<Message> {
        self.pubsub.clone()
    }

    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            id: Sid::new(),
            pubsub: PubSub(Arc::new(tx)),
            register: HashMap::new(),
            rx,
        }
    }

    pub async fn run(self) {
        let mut register = self.register;
        let mut rx = self.rx;
        while let Some(message) = rx.recv().await {
            match message {
                Message::Register(user_id, ws, who) => {
                    let (tx, mut rx) = ws.split();
                    let pubsub = self.pubsub.clone();
                    let listener = tokio::spawn(async move {
                        while let Some(Ok(msg)) = rx.next().await {
                            match msg {
                                WsMessage::Close(_) => {
                                    println!("Close frame received from {who}");
                                    break;
                                }
                                // TODO handle received commands here
                                _ => println!("Received message from {who}"),
                            }
                        }
                        pubsub.unsubscribe(user_id, who).unwrap();
                    });
                    register.entry(user_id).or_insert(Vec::new()).push(SocketClient {
                        who,
                        tx,
                        listener,
                    });
                }
                Message::Message(user_id, message) => {
                    let Some(clients) = register.get_mut(&user_id) else {
                        continue;
                    };
                    for client in clients {
                        let Err(e) = client.tx.send(WsMessage::Text(message.clone())).await else {
                            continue;
                        };
                        let who = client.who;
                        eprintln!("Error sending message to {who}: {e}");
                        self.pubsub.unsubscribe(user_id, who).unwrap();
                    }
                }
                Message::Unregister(user_id, who) => {
                    let Some(clients) = register.get_mut(&user_id) else {
                        eprintln!("No clients for {user_id}");
                        continue;
                    };
                    clients.retain(|c| {
                        let keep = c.who != who;
                        if !keep {
                            // c.tx will be closed when its dropped
                            c.listener.abort();
                        }
                        keep
                    });
                }
            }
        }
    }
}