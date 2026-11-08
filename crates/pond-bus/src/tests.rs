use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum T {
    Person,
    Task(u8),
    Chore(u8),
}

impl Lane for T {
    fn lane(self) -> u8 {
        match self {
            T::Person => 2,
            T::Task(_) => 1,
            T::Chore(_) => 0,
        }
    }
}

fn fresh() -> &'static Bus<T> {
    Box::leak(Box::new(Bus::new("test_bus")))
}

async fn now(bus: &'static Bus<T>, claim: T) -> Floor<T> {
    bus.claim(claim, Some(Duration::from_secs(5)), || true)
        .await
        .expect("a free bus is granted")
}

async fn until_waiting(bus: &Bus<T>, waiting: usize) {
    let mut seen = bus.subscribe();
    seen.wait_for(|s| s.waiting == waiting).await.unwrap();
}

fn waiter(
    bus: &'static Bus<T>,
    claim: T,
    order: Arc<Mutex<Vec<T>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let floor = bus.claim(claim, None, || true).await.unwrap();
        order.lock().unwrap().push(floor.claim());
    })
}

#[tokio::test]
async fn a_lane_is_served_in_the_order_its_claims_asked() {
    let bus = fresh();
    let holder = now(bus, T::Chore(0)).await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let a = waiter(bus, T::Chore(1), order.clone());
    until_waiting(bus, 1).await;
    let b = waiter(bus, T::Chore(2), order.clone());
    until_waiting(bus, 2).await;
    drop(holder);
    a.await.unwrap();
    b.await.unwrap();
    assert_eq!(*order.lock().unwrap(), vec![T::Chore(1), T::Chore(2)]);
}

#[tokio::test]
async fn a_person_then_a_task_go_before_the_chores_that_asked_first() {
    let bus = fresh();
    let holder = now(bus, T::Chore(0)).await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let chore = waiter(bus, T::Chore(1), order.clone());
    until_waiting(bus, 1).await;
    let task = waiter(bus, T::Task(1), order.clone());
    until_waiting(bus, 2).await;
    let person = waiter(bus, T::Person, order.clone());
    until_waiting(bus, 3).await;
    drop(holder);
    for w in [chore, task, person] {
        w.await.unwrap();
    }
    assert_eq!(
        *order.lock().unwrap(),
        vec![T::Person, T::Task(1), T::Chore(1)]
    );
}

#[tokio::test]
async fn a_higher_lane_waiting_asks_the_holder_to_yield_and_a_lower_one_does_not() {
    let bus = fresh();
    let task = now(bus, T::Task(0)).await;
    let chore = tokio::spawn(bus.claim(T::Chore(0), Some(Duration::from_millis(200)), || true));
    until_waiting(bus, 1).await;
    assert!(task.keep(), "a chore waits its turn");
    let person = tokio::spawn(bus.claim(T::Person, None, || true));
    until_waiting(bus, 2).await;
    assert!(!task.keep(), "a person waiting asks the task to yield");
    tokio::time::timeout(Duration::from_secs(1), task.yield_requested())
        .await
        .expect("the yield is announced");
    drop(task);
    assert_eq!(person.await.unwrap().map(|f| f.claim()), Some(T::Person));
    drop(chore);
}

#[tokio::test]
async fn a_claim_no_longer_wanted_leaves_the_queue() {
    let bus = fresh();
    let _holder = now(bus, T::Task(0)).await;
    let wanted = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let still = wanted.clone();
    let waiting = tokio::spawn(bus.claim(T::Task(1), None, move || still.load(Ordering::SeqCst)));
    until_waiting(bus, 1).await;
    wanted.store(false, Ordering::SeqCst);
    assert!(waiting.await.unwrap().is_none());
    assert_eq!(bus.state().waiting, 0);
}

#[tokio::test]
async fn a_claim_whose_waiter_goes_away_leaves_the_queue() {
    let bus = fresh();
    let holder = now(bus, T::Task(0)).await;
    let waiting = tokio::spawn(bus.claim(T::Person, None, || true));
    until_waiting(bus, 1).await;
    waiting.abort();
    let _ = waiting.await;
    assert_eq!(bus.state().waiting, 0, "an aborted claim keeps no place");
    drop(holder);
    let next = bus
        .claim(T::Chore(0), Some(Duration::from_secs(1)), || true)
        .await;
    assert!(next.is_some(), "nobody ghost holds the head of the queue");
}

#[tokio::test]
async fn the_bus_tells_its_subscribers_who_holds_it_and_who_is_next() {
    let bus = fresh();
    let mut seen = bus.subscribe();
    assert!(seen.borrow().is_free());
    let task = now(bus, T::Task(0)).await;
    seen.wait_for(|s| s.holder == Some(T::Task(0)))
        .await
        .unwrap();
    let person = tokio::spawn(bus.claim(T::Person, None, || true));
    let state = *seen.wait_for(|s| s.next == Some(T::Person)).await.unwrap();
    assert!(state.has(|c| c == T::Person), "a person waiting is visible");
    drop(task);
    let person = person.await.unwrap().unwrap();
    seen.wait_for(|s| s.holder == Some(T::Person) && s.waiting == 0)
        .await
        .unwrap();
    drop(person);
    seen.wait_for(BusState::is_free).await.unwrap();
}

#[tokio::test]
async fn a_held_floor_gives_way_between_bursts_and_queues_again_for_the_next() {
    let bus = fresh();
    let step = HeldFloor::new(bus, T::Task(0));
    let way = tokio::spawn({
        let step = step.clone();
        async move { step.give_way().await }
    });
    drop(step.burst(|| true).await.expect("a free bus"));
    assert_eq!(bus.state().holder, Some(T::Task(0)), "held between bursts");

    let person = tokio::spawn(bus.claim(T::Person, None, || true));
    let person = person.await.unwrap().expect("the idle step gave way");
    assert!(!step.holds());

    let next = tokio::spawn({
        let step = step.clone();
        async move { step.burst(|| true).await.map(|b| b.keep()) }
    });
    until_waiting(bus, 1).await;
    drop(person);
    assert_eq!(
        next.await.unwrap(),
        Some(true),
        "it queued and got the bus back"
    );
    way.abort();
}

#[tokio::test]
async fn a_held_floor_asked_mid_burst_gives_way_when_the_burst_ends() {
    let bus = fresh();
    let step = HeldFloor::new(bus, T::Task(0));
    let way = tokio::spawn({
        let step = step.clone();
        async move { step.give_way().await }
    });
    let burst = step.burst(|| true).await.unwrap();
    let person = tokio::spawn(bus.claim(T::Person, None, || true));
    until_waiting(bus, 1).await;
    assert!(
        !burst.keep(),
        "the burst is asked to stop at its next token"
    );
    assert_eq!(
        bus.state().holder,
        Some(T::Task(0)),
        "but it owns the bus until it ends"
    );
    drop(burst);
    assert!(person.await.unwrap().is_some());
    way.abort();
}

#[tokio::test]
async fn a_lower_claim_never_takes_a_held_floor_between_its_bursts() {
    let bus = fresh();
    let step = HeldFloor::new(bus, T::Task(0));
    drop(step.burst(|| true).await.unwrap());
    let chore = bus
        .claim(T::Chore(0), Some(Duration::from_millis(100)), || true)
        .await;
    assert!(chore.is_none(), "a chore waits for the step's whole run");
    assert!(step.holds());
}
