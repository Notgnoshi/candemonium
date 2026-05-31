use std::collections::HashMap;
use std::io::Cursor;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Sender;
use neli::FromBytesWithInput;
use neli::consts::nl::NlmF;
use neli::consts::rtnl::{Iff, Ifla, RtAddrFamily, Rtm};
use neli::consts::socket::NlFamily;
use neli::nl::{NlPayload, NlmsghdrBuilder};
use neli::rtnl::{Ifinfomsg, IfinfomsgBuilder};
use neli::socket::synchronous::NlSocketHandle;
use neli::types::Buffer;
use neli::utils::Groups;

const RTNLGRP_LINK: u32 = 1;

/// A link-state transition for one configured interface, keyed by [sock_id](crate::frame::CanFrame::sock_id).
#[derive(Clone, Copy, Debug)]
pub enum LinkEvent {
    LinkUp { sock_id: usize },
    LinkDown { sock_id: usize },
}

/// Watch link state for the configured `names`, sending a [LinkEvent] on each up/down transition.
///
/// Intended to run on a dedicated thread: it blocks while idle and returns when the given `stop`
/// flag is set, the `events` receiver is dropped, or the netlink socket fails.
pub fn run(stop: &AtomicBool, names: &[String], events: &Sender<LinkEvent>) -> eyre::Result<()> {
    let name_to_sock_id: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(sock_id, name)| (name.as_str(), sock_id))
        .collect();

    let socket =
        NlSocketHandle::connect(NlFamily::Route, None, Groups::new_groups(&[RTNLGRP_LINK]))?;

    let dump = NlmsghdrBuilder::default()
        .nl_type(Rtm::Getlink)
        .nl_flags(NlmF::REQUEST | NlmF::DUMP)
        .nl_payload(NlPayload::Payload(
            IfinfomsgBuilder::default()
                .ifi_family(RtAddrFamily::Unspecified)
                .build()
                .map_err(|e| eyre::eyre!("{e}"))?,
        ))
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;
    socket.send(&dump)?;

    let mut pollfd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };

    while !stop.load(Ordering::Relaxed) {
        pollfd.revents = 0;
        let ret = unsafe { libc::poll(&mut pollfd, 1, 100) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err.into());
        }
        if ret == 0 {
            continue;
        }
        if pollfd.revents & libc::POLLIN == 0 {
            return Err(eyre::eyre!(
                "netlink socket unusable (poll revents={})",
                pollfd.revents
            ));
        }

        let (iter, _groups) = socket.recv::<u16, Buffer>()?;
        for msg in iter {
            let msg = msg?;
            let present = match *msg.nl_type() {
                libc::RTM_NEWLINK => true,
                libc::RTM_DELLINK => false,
                _ => continue,
            };
            let Some(payload) = msg.get_payload() else {
                continue;
            };
            let bytes: &[u8] = payload.as_ref();
            let ifi = Ifinfomsg::from_bytes_with_input(&mut Cursor::new(bytes), bytes.len())?;
            let Some(name) = ifname(&ifi) else { continue };
            let Some(&sock_id) = name_to_sock_id.get(name) else {
                continue;
            };

            let is_up = present && ifi.ifi_flags().contains(Iff::UP | Iff::LOWERUP);
            let event = if is_up {
                LinkEvent::LinkUp { sock_id }
            } else {
                LinkEvent::LinkDown { sock_id }
            };
            if events.send(event).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Extract the interface name (IFLA_IFNAME) from a parsed link message, if present.
fn ifname(ifi: &Ifinfomsg) -> Option<&str> {
    for attr in ifi.rtattrs().as_ref() {
        if *attr.rta_type() == Ifla::Ifname {
            let bytes: &[u8] = attr.rta_payload().as_ref();
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            return std::str::from_utf8(&bytes[..end]).ok();
        }
    }
    None
}
