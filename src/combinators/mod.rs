use crate::*;
use futures::future::select_all;
use never::Never;
use std::time::Duration;
use std::{future::Future, time::Instant};
use tokio::{
    self, select,
    time::{sleep, sleep_until},
};

pub fn map<E, I, O, F, Fut>(source: E, mut f: F) -> Eventual<O>
where
    E: IntoReader<Output = I>,
    F: 'static + Send + FnMut(I) -> Fut,
    I: Value,
    O: Value,
    Fut: Send + Future<Output = O>,
{
    let mut source = source.into_reader();

    Eventual::spawn(|mut writer| async move {
        loop {
            writer.write(f(source.next().await?).await);
        }
    })
}

pub fn timer(interval: Duration) -> Eventual<Instant> {
    Eventual::spawn(move |mut writer| async move {
        loop {
            writer.write(Instant::now());
            sleep(interval).await;
        }
    })
}

pub trait Joinable {
    type Output;
    fn join(self) -> Eventual<Self::Output>;
}

macro_rules! impl_join {
    ($len:expr, $($T:ident, $t:ident),*) => {
        impl<$($T,)*> Joinable for ($($T,)*)
            where
                $($T: IntoReader,)*
        {
            type Output = ($($T::Output),*);

            #[allow(non_snake_case)]
            fn join(self) -> Eventual<Self::Output> {
                let ($($T),*) = self;
                $(let mut $T = $T.into_reader();)*

                Eventual::spawn(move |mut writer| async move {
                    // In the first section we wait until all values are available
                    let mut count:usize = 0;
                    $(let mut $t = None;)*
                    let ($(mut $t,)*) = loop {
                        select! {
                            $(
                                next = $T.next() => {
                                    if $t.replace(next?).is_none() {
                                        count += 1;
                                    }
                                }
                            )*
                        }
                        if count == 2 {
                            break ($($t.unwrap()),*);
                        }
                    };
                    // Once all values are available, start writing but continue
                    // to update.
                    loop {
                        writer.write(($($t.clone(),)*));

                        select! {
                            $(
                                next = $T.next() => {
                                    $t = next?;
                                }
                            )*
                        }
                    }
                })
            }
        }
    };
}

macro_rules! impl_joins {
    ($len:expr, $A:ident, $a:ident) => { };
    ($len:expr, $A:ident, $a:ident, $($T:ident, $t:ident),+) => {
        impl_join!($len, $A, $a, $($T, $t),+);
        impl_joins!($len - 1, $($T, $t),+);
    }
}

impl_joins!(12, A, a, B, b, C, c, D, d, E, e, F, f, G, g, H, h, I, i, J, j, K, k, L, l);

pub fn join<J>(joinable: J) -> Eventual<J::Output>
where
    J: Joinable,
{
    joinable.join()
}

pub trait Selectable {
    type Output;
    fn select(self) -> Eventual<Self::Output>;
}

pub fn select<S>(selectable: S) -> Eventual<S::Output>
where
    S: Selectable,
{
    selectable.select()
}

impl<R> Selectable for Vec<R>
where
    R: IntoReader,
{
    type Output = R::Output;
    fn select(self) -> Eventual<Self::Output> {
        // TODO: With specialization we can avoid what is essentially an
        // unnecessary clone when R is EventualReader
        let mut readers: Vec<_> = self.into_iter().map(|v| v.into_reader()).collect();
        Eventual::spawn(move |mut writer| async move {
            loop {
                if readers.len() == 0 {
                    return Err(Closed);
                }
                let read_futs: Vec<_> = readers.iter_mut().map(|r| r.next()).collect();

                let (output, index, remainder) = select_all(read_futs).await;

                // Ideally, we would want to re-use this list, but in most
                // cases we can't because it may have been shuffled.
                drop(remainder);

                match output {
                    Ok(value) => {
                        writer.write(value);
                    }
                    Err(Closed) => {
                        readers.remove(index);
                    }
                }
            }
        })
    }
}

impl<T, A, B> Selectable for (A, B)
where
    A: IntoReader<Output = T>,
    B: IntoReader<Output = T>,
    // TODO: I don't understand why this bound is required.
    Vec<EventualReader<T>>: Selectable,
{
    type Output = <Vec<EventualReader<T>> as Selectable>::Output;
    fn select(self) -> Eventual<Self::Output> {
        let (a, b) = self;
        let a = a.into_reader();
        let b = b.into_reader();
        let ab = vec![a, b];
        ab.select()
    }
}

pub fn throttle<E>(read: E, duration: Duration) -> Eventual<E::Output>
where
    E: IntoReader,
{
    let mut read = read.into_reader();

    Eventual::spawn(move |mut writer| async move {
        loop {
            let mut next = read.next().await?;
            let end = tokio::time::Instant::now() + duration;
            loop {
                // Allow replacing the value until the time is up. This
                // necessarily introduces latency but de-duplicates when there
                // are intermittent bursts. Not sure what is better. Matching
                // common-ts for now.
                select! {
                    n = read.next() => {
                        next = n?;
                    }
                    _ = sleep_until(end) => {
                        break;
                    }
                }
            }
            writer.write(next);
        }
    })
}

/// Produce a side effect with the latest values of an eventual
pub fn pipe<E, F>(reader: E, mut f: F) -> PipeHandle
where
    E: IntoReader,
    F: 'static + Send + FnMut(E::Output),
{
    let mut reader = reader.into_reader();

    PipeHandle::new(Eventual::spawn(
        move |_writer: EventualWriter<Never>| async move {
            loop {
                f(reader.next().await?);
            }
        },
    ))
}

/// Pipe ceases when this is dropped
pub struct PipeHandle {
    _inner: Eventual<Never>,
}

impl PipeHandle {
    fn new(eventual: Eventual<Never>) -> Self {
        Self { _inner: eventual }
    }
}

pub fn handle_errors<E, F, Ok, Err>(source: E, mut f: F) -> Eventual<Ok>
where
    E: IntoReader<Output = Result<Ok, Err>>,
    F: 'static + Send + FnMut(Err),
    Ok: Value,
    Err: Value,
{
    let mut reader = source.into_reader();

    Eventual::spawn(move |mut writer| async move {
        loop {
            match reader.next().await? {
                Ok(v) => writer.write(v),
                Err(e) => f(e),
            }
        }
    })
}

// TODO: Retry. This is needed to be supported because retry should be eventual
// aware in that it will only retry if there is no update available, instead
// preferring the update. It's a little tricky to write in a general sense because
// it is not clear _what_ is being retried. A retry can't force an upstream map
// to produce a value again. You could couple the map and retry API, but that's
// not great. The only thing I can think of is to have a function produce an eventual
// upon encountering an error. That seems like the right choice but need to let it simmer.
//
// Below is an "interesting" first attempt.
//
// This is a retry that is maximimally abstracted.
// It is somewhat experimental, but makes sense if you
// want to be able to not tie retry down to any particular
// other feature (like map). It's also BONKERS. See map_with_rety
// for usage.
pub fn retry<Ok, Err, F, Fut>(mut f: F) -> Eventual<Ok>
where
    Ok: Value,
    Err: Value,
    Fut: Send + Future<Output = Eventual<Result<Ok, Err>>>,
    F: 'static + Send + FnMut(Option<Err>) -> Fut,
{
    Eventual::spawn(move |mut writer| async move {
        loop {
            let mut e = f(None).await.subscribe();
            let mut next = e.next().await;

            loop {
                match next? {
                    Ok(v) => {
                        writer.write(v);
                        next = e.next().await;
                    }
                    Err(err) => {
                        select! {
                            e_temp = f(Some(err)) => {
                                e = e_temp.subscribe();
                                next = e.next().await;
                            }
                            n_temp = e.next() => {
                                next = n_temp;
                            }
                        }
                    }
                }
            }
        }
    })
}

pub fn map_with_retry<I, Ok, Err, F, Fut, E, FutE>(
    source: Eventual<I>,
    f: F,
    on_err: E,
) -> Eventual<Ok>
where
    F: 'static + Clone + Send + Fn(I) -> Fut,
    E: 'static + Clone + Send + Sync + Fn(Err) -> FutE,
    I: Value,
    Ok: Value,
    Err: Value,
    Fut: Send + Future<Output = Result<Ok, Err>>,
    FutE: Send + Future<Output = ()>,
{
    retry(move |e| {
        let reader = source.subscribe();
        let f = f.clone();
        let on_err = on_err.clone();
        async move {
            if let Some(e) = e {
                on_err(e).await;
            }
            map(reader, f)
        }
    })
}
