//! A context based on tokio and asynchronous IO.
//!
//! Although supported by an asynchronous reactor, protocol code, i.e.,
//! `impl Receivers` is still synchronous and running in a separated thread.

use std::{borrow::Borrow, collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use bincode::Options;
use rand::Rng;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{net::UdpSocket, runtime::Handle, sync::Mutex, task::JoinHandle};
use tokio_util::bytes::Bytes;

use crate::context::crypto::Verifier;

use super::{
    crypto::{DigestHash, Sign, Signer, Verify},
    ordered_multicast::{OrderedMulticast, Variant},
    Addr, MultiplexReceive, OrderedMulticastReceive, To,
};

#[derive(Debug, Clone)]
enum Event {
    Message(SocketAddr, SocketAddr, Vec<u8>),
    LoopbackMessage(SocketAddr, Bytes),
    OrderedMulticastMessage(SocketAddr, Vec<u8>),
    Timer(SocketAddr, TimerId),
    TimerNotification,
    Stop,
}

pub struct Context<M> {
    socket: Arc<UdpSocket>,
    runtime: Handle,
    pub source: SocketAddr,
    signer: Arc<Signer>,
    timer_id: TimerId,
    timer_tasks: HashMap<TimerId, JoinHandle<()>>,
    timer_lock: Arc<Mutex<Vec<Event>>>,
    event: flume::Sender<Event>,
    rdv_event: flume::Sender<Event>,
    get_buf: Box<dyn Fn(M) -> Vec<u8> + Send + Sync>,
}

trait GetBuf<M> {
    fn get_buf(&self, message: M) -> Vec<u8>
    where
        M: Serialize;
}

struct Bincode<M, N>(std::marker::PhantomData<(M, N)>);

impl<M, N> GetBuf<N> for Bincode<M, N>
where
    N: Into<M>,
    M: Serialize + 'static,
{
    fn get_buf(&self, message: N) -> Vec<u8>
    where
        N: Serialize,
    {
        bincode::options().serialize(&message.into()).unwrap()
    }
}

impl<M> std::fmt::Debug for Context<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}(..)", std::any::type_name::<Self>())
    }
}

impl<M> Context<M> {
    pub fn send<N>(&self, to: To, message: N)
    where
        M: Sign<N>,
    {
        // println!("{to:?}");
        let message = M::sign(message, &self.signer);
        let buf = Bytes::from((self.get_buf)(message));
        // println!("{buf:02x?}");
        if matches!(
            to,
            // disallow root context to send to upcall?
            To::Loopback | To::AddrsWithLoopback(_) | To::Addr(Addr::Upcall)
        ) {
            self.event
                .send(Event::LoopbackMessage(self.source, buf.clone()))
                .unwrap()
        }
        match to {
            To::Addr(Addr::Upcall) => {}
            To::Addr(addr) => self.send_buf(addr, buf.clone()),
            To::Addrs(addrs) | To::AddrsWithLoopback(addrs) => {
                for addr in addrs {
                    self.send_buf(addr, buf.clone())
                }
            }
            To::Loopback => {}
        }
    }

    pub fn send_buf(&self, addr: Addr, buf: impl AsRef<[u8]> + Send + Sync + 'static) {
        let Addr::Socket(addr) = addr else {
            unimplemented!()
        };
        let socket = self.socket.clone();
        self.runtime.spawn(async move {
            socket
                .send_to(buf.as_ref(), addr)
                .await
                .unwrap_or_else(|err| panic!("{err} target: {addr:?}"))
        });
    }

    pub fn idle_hint(&self) -> bool {
        self.event.is_empty()
    }
}

pub type TimerId = (u32, u32); // (subnode id, local sequence number)

// timer is designed to eliminate false alarm through rendezvous channel
// however, current flume implementation of rendezvous channel is buggy and
// does not maintain semantic
// so `Event::Timer` are passed through a temporary locked vector, and the
// channel instead passes a temporary `Event::TimerNotification` which is
// allowed to be spurious
// i believe the original solution should also work for multithreading runtime

impl<M> Context<M> {
    pub fn set(&mut self, duration: Duration) -> TimerId {
        self.timer_id.1 += 1;
        let id = self.timer_id;
        let event = self.rdv_event.clone();
        let source = self.source;
        let timer_lock = self.timer_lock.clone();
        let task = self.runtime.spawn(async move {
            loop {
                tokio::time::sleep(duration).await;
                timer_lock.lock().await.push(Event::Timer(source, id));
                event.send_async(Event::TimerNotification).await.unwrap()
            }
        });
        self.timer_tasks.insert(id, task);
        id
    }

    pub fn unset(&mut self, id: TimerId) {
        let task = self.timer_tasks.remove(&id).unwrap();
        task.abort();
        let result = self.runtime.block_on(task);
        assert!(result.is_err())
    }
}

#[derive(Debug)]
pub struct Multiplex {
    runtime: Handle,
    variant: Arc<Variant>,
    event: (flume::Sender<Event>, flume::Receiver<Event>),
    rdv_event: (flume::Sender<Event>, flume::Receiver<Event>),
    timer_lock: Arc<Mutex<Vec<Event>>>,
    subnode_id: u32,
    pub drop_rate: f64,
}

impl Multiplex {
    pub fn new(runtime: Handle, variant: impl Into<Arc<Variant>>) -> Self {
        Self {
            runtime,
            variant: variant.into(),
            event: flume::unbounded(),
            rdv_event: flume::bounded(0),
            timer_lock: Default::default(),
            subnode_id: Default::default(),
            drop_rate: 0.,
        }
    }

    pub fn register<M>(&self, addr: Addr, signer: impl Into<Arc<Signer>>) -> super::Context<M>
    where
        M: Serialize,
    {
        let Addr::Socket(addr) = addr else {
            unimplemented!()
        };
        let socket = Arc::new(
            self.runtime
                .block_on(UdpSocket::bind(addr))
                .unwrap_or_else(|_| panic!("binding {addr:?}")),
        );
        socket.set_broadcast(true).unwrap();
        let context = Context {
            socket: socket.clone(),
            runtime: self.runtime.clone(),
            source: addr,
            signer: signer.into(),
            timer_id: Default::default(),
            timer_tasks: Default::default(),
            timer_lock: self.timer_lock.clone(),
            event: self.event.0.clone(),
            rdv_event: self.rdv_event.0.clone(),
            get_buf: Box::new(|message| bincode::options().serialize(&message).unwrap()),
        };
        let event = self.event.0.clone();
        self.runtime.spawn(async move {
            let mut buf = vec![0; 65536];
            loop {
                let (len, remote) = socket.recv_from(&mut buf).await.unwrap();
                // println!("{:02x?}", &buf[..len]);
                // `try_send` here to minimize rx process latency, avoid hardware packet dropping
                event
                    .try_send(Event::Message(addr, remote, buf[..len].to_vec()))
                    .unwrap()
            }
        });
        super::Context::Tokio(context)
    }

    pub fn register_subnode<M, N>(&mut self, context: &super::Context<M>) -> super::Context<N>
    where
        N: Into<M>,
        M: Serialize,
    {
        let super::Context::Tokio(context) = context else {
            unimplemented!()
        };
        self.subnode_id += 1;
        super::Context::Tokio(Context {
            socket: context.socket.clone(),
            runtime: self.runtime.clone(),
            source: context.source,
            signer: context.signer.clone(),
            timer_id: (self.subnode_id, Default::default()),
            timer_tasks: Default::default(),
            timer_lock: self.timer_lock.clone(),
            event: self.event.0.clone(),
            rdv_event: self.rdv_event.0.clone(),
            get_buf: Box::new(|message| bincode::options().serialize(&message.into()).unwrap()),
        })
    }
}

impl Multiplex {
    fn run_internal<R, M, N, I>(
        &self,
        receive: &mut R,
        from_ordered_multicast: impl Fn(OrderedMulticast<N>) -> M,
        verifier: &Verifier<I>,
    ) where
        R: MultiplexReceive<Message = M>,
        M: DeserializeOwned + Verify<I>,
        N: DeserializeOwned + DigestHash,
    {
        let deserialize = |buf: &_| {
            bincode::options()
                .allow_trailing_bytes()
                .deserialize::<M>(buf)
                .unwrap()
        };
        let mut delegate = self.variant.delegate();
        let mut pace_count = 1;
        loop {
            if pace_count == 0 {
                // println!("* pace");
                delegate.on_pace(receive, verifier, &from_ordered_multicast);
                receive.on_pace();
                pace_count = if self.event.0.is_empty() {
                    1
                } else {
                    self.event.0.len()
                };
                // println!("* pace count {pace_count}");
            }

            assert!(self.event.1.len() < 4096, "receivers overwhelmed");
            let event = flume::Selector::new()
                .recv(&self.event.1, Result::unwrap)
                .recv(&self.rdv_event.1, Result::unwrap)
                .wait();
            // println!("{event:?}");
            let mut timer_lock = self.timer_lock.blocking_lock();
            for event in timer_lock.drain(..) {
                let Event::Timer(receiver, id) = event else {
                    unreachable!()
                };
                receive.on_timer(Socket(receiver), super::TimerId::Tokio(id))
            }

            use crate::context::Addr::Socket;
            match event {
                Event::Stop => break,
                Event::Message(receiver, remote, message) => {
                    pace_count -= 1;
                    if self.drop_rate != 0. && rand::thread_rng().gen_bool(self.drop_rate) {
                        continue;
                    }
                    let message = deserialize(&message);
                    message.verify(verifier).unwrap();
                    receive.handle(Socket(receiver), Socket(remote), message)
                }
                Event::LoopbackMessage(receiver, message) => {
                    pace_count -= 1;
                    receive.handle_loopback(Socket(receiver), deserialize(&message))
                }
                Event::OrderedMulticastMessage(remote, message) => {
                    pace_count -= 1;
                    if self.drop_rate != 0. && rand::thread_rng().gen_bool(self.drop_rate) {
                        continue;
                    }
                    delegate.handle(
                        Socket(remote),
                        self.variant.deserialize(message),
                        receive,
                        verifier,
                        &from_ordered_multicast,
                    )
                }
                Event::TimerNotification => {} // handled above
                Event::Timer(_, _) => unreachable!(),
            }
        }
    }

    pub fn run<M, I>(
        &self,
        receivers: &mut impl MultiplexReceive<Message = M>,
        verifier: impl Borrow<Verifier<I>>,
    ) where
        M: DeserializeOwned + Verify<I>,
    {
        #[derive(Deserialize)]
        enum O {}
        impl DigestHash for O {
            fn hash(&self, _: &mut impl std::hash::Hasher) {
                unreachable!()
            }
        }
        self.run_internal::<_, _, O, _>(receivers, |_| unimplemented!(), verifier.borrow())
    }
}

#[derive(Debug)]
pub struct OrderedMulticastMultiplex(Multiplex);

impl std::ops::Deref for OrderedMulticastMultiplex {
    type Target = Multiplex;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Multiplex {
    pub fn enable_ordered_multicast(self, addr: Addr) -> OrderedMulticastMultiplex {
        let Addr::Socket(addr) = addr else {
            unimplemented!()
        };
        let socket = self
            .runtime
            // .block_on(UdpSocket::bind(self.config.multicast_addr.unwrap()))
            .block_on(UdpSocket::bind(("0.0.0.0", addr.port())))
            .unwrap();
        let event = self.event.0.clone();
        self.runtime.spawn(async move {
            let mut buf = vec![0; 65536];
            loop {
                let (len, remote) = socket.recv_from(&mut buf).await.unwrap();
                event
                    .try_send(Event::OrderedMulticastMessage(remote, buf[..len].to_vec()))
                    .unwrap()
            }
        });
        OrderedMulticastMultiplex(self)
    }
}

impl OrderedMulticastMultiplex {
    pub fn run<M, N, I>(
        &self,
        receivers: &mut (impl MultiplexReceive<Message = M> + OrderedMulticastReceive<Message = N>),
        verifier: impl Borrow<Verifier<I>>,
    ) where
        M: DeserializeOwned + Verify<I>,
        N: DeserializeOwned + DigestHash,
        OrderedMulticast<N>: Into<M>,
    {
        self.run_internal(receivers, Into::into, verifier.borrow())
    }
}

pub struct MultiplexHandle {
    stop: Box<dyn Fn() + Send + Sync>,
    stop_async:
        Box<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> + Send + Sync>,
}

impl Multiplex {
    pub fn handle(&self) -> MultiplexHandle {
        MultiplexHandle {
            stop: Box::new({
                let rdv_event = self.rdv_event.0.clone();
                move || rdv_event.send(Event::Stop).unwrap()
            }),
            stop_async: Box::new({
                let rdv_event = self.rdv_event.0.clone();
                Box::new(move || {
                    let rdv_event = rdv_event.clone();
                    Box::pin(async move { rdv_event.send_async(Event::Stop).await.unwrap() }) as _
                })
            }),
        }
    }
}

impl MultiplexHandle {
    pub fn stop(&self) {
        (self.stop)()
    }

    pub async fn stop_async(&self) {
        (self.stop_async)().await
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    fn false_alarm() {
        // let runtime = tokio::runtime::Builder::new_multi_thread()
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let addr = SocketAddr::from(([127, 0, 0, 1], 10000));
        let multiplex = Multiplex::new(runtime.handle().clone(), Variant::Unreachable);

        #[derive(Serialize, Deserialize)]
        struct M;
        impl<I> Verify<I> for M {
            fn verify(&self, _: &Verifier<I>) -> Result<(), crate::context::crypto::Invalid> {
                Ok(())
            }
        }

        let mut context = multiplex.register(Addr::Socket(addr), Signer::new_standard(None));
        let id = context.set(Duration::from_millis(10));

        let handle = multiplex.handle();
        let event = multiplex.event.0.clone();
        std::thread::spawn(move || {
            runtime.block_on(async move {
                tokio::time::sleep(Duration::from_millis(9)).await;
                event
                    .send_async(Event::Message(
                        addr,
                        SocketAddr::from(([127, 0, 0, 1], 20000)),
                        bincode::options().serialize(&M).unwrap(),
                    ))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
                handle.stop_async().await;
            });
            runtime.shutdown_background();
        });

        struct R(bool, crate::context::Context<M>, crate::context::TimerId);
        impl MultiplexReceive for R {
            type Message = M;

            fn handle(
                &mut self,
                _: crate::context::Addr,
                _: crate::context::Addr,
                M: Self::Message,
            ) {
                if !self.0 {
                    println!("unset");
                    self.1.unset(self.2);
                }
                self.0 = true;
            }

            fn handle_loopback(&mut self, _: crate::context::Addr, _: Self::Message) {
                unreachable!()
            }

            fn on_timer(&mut self, _: crate::context::Addr, _: crate::context::TimerId) {
                println!("alarm");
                assert!(!self.0);
            }
        }

        multiplex.run(&mut R(false, context, id), Verifier::<()>::Nop);
    }

    #[test]
    fn false_alarm_100() {
        for _ in 0..100 {
            false_alarm();
            println!()
        }
    }
}
