//! The contact cache against a model of the firmware's contact table: whatever
//! comes in, once the contacts are read the cache holds what the device holds.

mod common;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use common::{
    Device, Random, client_on_device, device_info_frame, err, ok, poll_once, self_info_frame,
};
use meshcore::{Contact, ContactType, MeshCore, Options, PathLen, PublicKey};

/// How many contacts the device holds; a new one takes the oldest one's place.
const MAX_CONTACTS: usize = 10;

/// What happens besides adverts, path updates, messages and commands.
#[derive(Clone, Copy, Default)]
struct World {
    /// The link drops, and the client reconnects.
    links_drop: bool,
    /// The device's clock is set back.
    clock_set_back: bool,
    /// Another app, or someone at the device, adds and removes contacts. The
    /// device does not report that.
    others: bool,
    /// The device counts this many entries at most that it does not list, as
    /// firmware 1.16.0 does.
    unlisted: u64,
    /// One of this many commands about a contact gets no answer (0: none).
    unanswered: u64,
}

/// The contact table of the firmware (`BaseChatMesh`, `MyMesh`).
struct Table {
    device: Option<Device>,
    contacts: Vec<Contact>,
    clock: u32,
    adverts: u32,
    /// The list the device is sending.
    list: Option<List>,
    /// How many entries it counts but does not list.
    unlisted: u32,
    world: World,
    luck: Random,
}

struct List {
    since: u32,
    next: usize,
    newest: u32,
}

fn lock(table: &Mutex<Table>) -> MutexGuard<'_, Table> {
    table.lock().unwrap()
}

/// A contact as the device sends it.
fn frame(code: u8, contact: &Contact) -> Vec<u8> {
    let mut path = contact.out_path.clone();
    path.resize(64, 0);
    [
        &[code][..],
        contact.public_key.as_bytes(),
        &[
            contact.contact_type.code(),
            contact.flags,
            contact.out_path_len.as_byte(),
        ],
        &path,
        &contact.name_raw,
        &contact.last_advert.to_le_bytes(),
        &contact.latitude_e6.to_le_bytes(),
        &contact.longitude_e6.to_le_bytes(),
        &contact.lastmod.to_le_bytes(),
    ]
    .concat()
}

/// The contact a `CMD_ADD_UPDATE_CONTACT` writes.
fn written(command: &[u8], clock: u32) -> Contact {
    let key = PublicKey::from_slice(&command[1..33]).unwrap();
    let name = std::str::from_utf8(&command[100..132]).unwrap();
    let mut contact = Contact::new(key, ContactType::from_code(command[33]), name);
    contact.flags = command[34];
    contact.out_path_len = PathLen::from_byte(command[35]);
    contact.out_path = command[36..36 + contact.out_path_len.byte_len()].to_vec();
    contact.last_advert = u32::from_le_bytes(command[132..136].try_into().unwrap());
    contact.lastmod = clock;
    contact
}

impl Table {
    /// Sends a frame, if there is a link to send it on.
    fn send(&self, frame: Vec<u8>) {
        if let Some(device) = &self.device {
            device.try_push(frame);
        }
    }

    fn index(&self, key: &[u8]) -> Option<usize> {
        self.contacts
            .iter()
            .position(|contact| contact.public_key.as_bytes()[..] == *key)
    }

    /// Answers a command.
    fn command(&mut self, command: &[u8]) {
        let found = command.get(1..33).and_then(|key| self.index(key));
        let reply = match (command[0], found) {
            (1, _) => {
                self.list = None;
                self_info_frame("Node")
            }
            (22, _) => device_info_frame(0),
            (4, _) if self.list.is_some() => err(4),
            (4, _) => {
                let since = command
                    .get(1..5)
                    .map_or(0, |since| u32::from_le_bytes(since.try_into().unwrap()));
                self.list = Some(List {
                    since,
                    next: 0,
                    newest: 0,
                });
                let count = u32::try_from(self.contacts.len()).unwrap() + self.unlisted;
                [&[2][..], &count.to_le_bytes()].concat()
            }
            (9, Some(at)) => {
                self.contacts[at] = written(command, self.clock);
                ok()
            }
            (9, None) => {
                self.add(written(command, self.clock));
                ok()
            }
            (13, Some(at)) => {
                // without a new lastmod
                self.contacts[at].out_path_len = PathLen::NONE;
                self.contacts[at].out_path.clear();
                ok()
            }
            (15, Some(at)) => {
                // the contacts after it move up, also under a list
                self.contacts.remove(at);
                ok()
            }
            (30, Some(at)) => frame(3, &self.contacts[at]),
            (13 | 15 | 30, None) => err(2),
            _ => err(1),
        };
        if self.world.links_drop && self.luck.below(40) == 0 {
            self.drop_link();
        } else if matches!(command[0], 9 | 13 | 15 | 30)
            && self.world.unanswered > 0
            && self.luck.below(self.world.unanswered) == 0
        {
            // the answer gets lost
        } else {
            self.send(reply);
        }
    }

    fn drop_link(&mut self) {
        if let Some(device) = &self.device {
            device.try_drop_link();
        }
        // what happens without a link, happens later
        self.clock += 1;
    }

    fn add(&mut self, contact: Contact) {
        if self.contacts.len() < MAX_CONTACTS {
            self.contacts.push(contact);
            return;
        }
        let oldest = self.contacts.iter_mut().min_by_key(|c| c.lastmod).unwrap();
        let key = std::mem::replace(oldest, contact).public_key;
        self.send([&[0x8f][..], key.as_bytes()].concat());
    }

    /// Goes on with the list: one contact, or its end.
    fn step(&mut self) {
        let Some(mut list) = self.list.take() else {
            return;
        };
        if let Some(contact) = self.contacts.get(list.next) {
            if contact.lastmod > list.since {
                list.newest = list.newest.max(contact.lastmod);
                self.send(frame(3, contact));
            }
            list.next += 1;
            self.list = Some(list);
        } else {
            self.send([&[4][..], &list.newest.to_le_bytes()].concat());
        }
    }

    fn advert(&mut self, key: PublicKey) {
        self.adverts += 1;
        let (name, clock) = (format!("advert {}", self.adverts), self.clock);
        let at = self.index(key.as_bytes()).unwrap_or_else(|| {
            self.add(Contact::new(key, ContactType::Chat, ""));
            self.index(key.as_bytes()).unwrap()
        });
        let contact = &mut self.contacts[at];
        contact.set_name(&name);
        contact.last_advert = self.adverts;
        contact.lastmod = clock;
        self.send([&[0x80][..], key.as_bytes()].concat());
    }

    /// One thing of many that may happen at any time.
    fn something_happens(&mut self, world: World, random: &mut Random) {
        let some = random.below(MAX_CONTACTS as u64) as usize;
        let clock = self.clock;
        let Some(contact) = self.contacts.get_mut(some) else {
            return self.advert(random.key());
        };
        match random.below(60) {
            0..4 => self.advert(random.key()),
            4..6 => {
                contact.out_path_len = PathLen::ZERO_HOP;
                contact.out_path = vec![];
                contact.lastmod = clock;
                let key = contact.public_key;
                self.send([&[0x81][..], key.as_bytes()].concat());
            }
            // a message: the contact was heard
            6 => contact.lastmod = clock,
            7..10 => self.clock += random.below(3) as u32,
            10 if world.clock_set_back => self.clock -= random.below(20) as u32,
            // Not under a list, which would leave out another contact: with
            // one added at the same time its count would still be right.
            11 if world.others && self.list.is_none() => {
                self.contacts.remove(some);
            }
            12 if world.others && self.contacts.len() < MAX_CONTACTS => {
                let key = random.key();
                if self.index(key.as_bytes()).is_none() {
                    // later than the newest contact of the last list
                    self.clock += 1;
                    let mut contact = Contact::new(key, ContactType::Room, "other");
                    contact.lastmod = self.clock;
                    self.contacts.push(contact);
                }
            }
            13 => self.unlisted = random.below(world.unlisted + 1) as u32,
            _ => {}
        }
    }
}

/// Something a program does with the client.
async fn use_client(mc: &MeshCore, random: &mut Random) {
    let key = random.key();
    let lastmod = mc.contacts_lastmod();
    match random.below(16) {
        0 => drop(mc.get_contacts(0).await),
        1 => drop(mc.get_contacts(lastmod).await),
        2 => drop(mc.get_contacts_async(0).await),
        3 => drop(mc.get_contacts_async(lastmod).await),
        4 | 5 => drop(mc.ensure_contacts(true).await),
        6 => drop(mc.ensure_contacts(false).await),
        7 => drop(mc.remove_contact(&key).await),
        8 => drop(mc.reset_path(&key).await),
        9 => {
            let contact = Contact::new(key, ContactType::Repeater, "written");
            drop(mc.add_contact(&contact).await);
        }
        10 => {
            if let Some(contact) = mc.contact(&key) {
                drop(mc.change_contact_flags(&contact, 2).await);
            }
        }
        11 => drop(mc.get_contact_by_key(&key).await),
        // not polled for a while, or given up
        12 | 15 => {
            let contact = Contact::new(key, ContactType::Repeater, "written");
            let kind = random.below(3);
            let mut command = Box::pin(async {
                match kind {
                    0 => mc.remove_contact(&key).await,
                    1 => mc.reset_path(&key).await,
                    _ => mc.add_contact(&contact).await,
                }
            });
            let done = poll_once(&mut command).await;
            for _ in 0..random.below(4) {
                tokio::task::yield_now().await;
            }
            if random.below(2) == 0 {
                tokio::time::sleep(random.millis(40)).await;
            }
            if !done && random.below(2) == 0 {
                drop(command.await);
            }
        }
        // given up, while the list goes on
        13 => drop(tokio::time::timeout(random.millis(30), mc.get_contacts(0)).await),
        _ => drop(tokio::time::timeout(random.millis(30), mc.ensure_contacts(true)).await),
    }
    tokio::time::sleep(random.millis(20)).await;
}

/// Contacts in the order of their keys, without the time of the last change
/// (the client does not learn it for a contact it writes).
fn comparable(contacts: &[Contact]) -> Vec<Contact> {
    let mut contacts = contacts.to_vec();
    contacts.sort_by_key(|contact| contact.public_key);
    for contact in &mut contacts {
        contact.lastmod = 0;
    }
    contacts
}

async fn check(seed: u64, world: World) {
    let table = Arc::new(Mutex::new(Table {
        device: None,
        contacts: Vec::new(),
        clock: 1_000_000,
        adverts: 0,
        list: None,
        unlisted: 0,
        world,
        luck: Random::new(seed * 10 + 9),
    }));
    let options = Options {
        auto_reconnect: true,
        default_timeout: Duration::from_millis(200),
        ..Options::default()
    };
    let (mc, device) = client_on_device(options, {
        let table = Arc::clone(&table);
        move |command| {
            lock(&table).command(command);
            vec![]
        }
    });
    lock(&table).device = Some(device.clone());
    while mc.connect().await.is_err() {}
    mc.set_auto_update_contacts(seed.is_multiple_of(2));
    let mesh = tokio::spawn({
        let table = Arc::clone(&table);
        let mut random = Random::new(seed * 10);
        async move {
            loop {
                {
                    let mut table = lock(&table);
                    if random.below(3) > 0 {
                        table.step();
                    }
                    table.something_happens(world, &mut random);
                    if world.links_drop && random.below(200) == 0 {
                        table.drop_link();
                    }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    });
    let programs: Vec<_> = (1..4)
        .map(|program| {
            let mc = mc.clone();
            let mut random = Random::new(seed * 10 + program);
            tokio::spawn(async move {
                for _ in 0..40 {
                    use_client(&mc, &mut random).await;
                }
            })
        })
        .collect();
    for program in programs {
        program.await.unwrap();
    }
    mesh.abort();
    // quiet now, but for the list the device may be sending
    let quiet = tokio::spawn({
        let table = Arc::clone(&table);
        async move {
            loop {
                lock(&table).step();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    });
    let mut random = Random::new(seed * 10 + 8);
    for _ in 0..5 {
        lock(&table).world = World::default();
        tokio::time::sleep(Duration::from_secs(8)).await;
        if world.others {
            // what others did shows with the next list
            mc.get_contacts(mc.contacts_lastmod()).await.unwrap();
        }
        let mut reads = 0;
        while mc.ensure_contacts(true).await.unwrap() {
            reads += 1;
            assert!(reads < 4, "seed {seed}: read again and again");
        }
        assert_eq!(
            comparable(&mc.contacts()),
            comparable(&lock(&table).contacts),
            "seed {seed}"
        );
        // what one use of the client changes, with nothing else to notice it by
        {
            let mut table = lock(&table);
            table.world = world;
            table.clock += random.below(2) as u32;
        }
        use_client(&mc, &mut random).await;
    }
    quiet.abort();
}

async fn check_in(world: World) {
    for seed in 1..=50 {
        check(seed, world).await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_cache_follows_the_device() {
    check_in(World::default()).await;
}

#[tokio::test(start_paused = true)]
async fn the_cache_follows_the_device_when_links_drop() {
    check_in(World {
        links_drop: true,
        ..World::default()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_cache_follows_a_device_whose_clock_is_set_back() {
    check_in(World {
        clock_set_back: true,
        ..World::default()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_cache_follows_a_device_others_change() {
    check_in(World {
        others: true,
        links_drop: true,
        ..World::default()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_cache_follows_a_device_whose_answers_get_lost() {
    check_in(World {
        unanswered: 10,
        ..World::default()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_cache_follows_a_device_that_counts_more_than_it_lists() {
    check_in(World {
        unlisted: 2,
        ..World::default()
    })
    .await;
}
