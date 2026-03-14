//! Netlink helpers for managing vcan interfaces using the `neli` crate.

use std::ffi::CString;

use eyre::{WrapErr, bail};
use neli::consts::nl::NlmF;
use neli::consts::rtnl::{Ifla, IflaInfo, RtAddrFamily, Rtm};
use neli::consts::socket::NlFamily;
use neli::nl::NlPayload;
use neli::router::synchronous::NlRouter;
use neli::rtnl::{Ifinfomsg, IfinfomsgBuilder, RtattrBuilder};
use neli::types::RtBuffer;
use neli::utils::Groups;

/// Create a vcan interface and bring it up.
pub fn create_vcan(name: &str) -> eyre::Result<()> {
    create_link(name).wrap_err_with(|| format!("creating vcan interface {name:?}"))?;
    set_link_up(name).wrap_err_with(|| format!("bringing up interface {name:?}"))
}

/// Delete a vcan interface. Silently succeeds if the interface is already gone.
pub fn delete_vcan(name: &str) -> eyre::Result<()> {
    let name_c = CString::new(name)?;
    let index = unsafe { libc::if_nametoindex(name_c.as_ptr()) };
    if index == 0 {
        return Ok(());
    }

    let msg = IfinfomsgBuilder::default()
        .ifi_family(RtAddrFamily::Unspecified)
        .ifi_index(index as i32)
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;

    send(Rtm::Dellink, NlmF::ACK, msg).wrap_err_with(|| format!("deleting interface {name:?}"))
}

fn create_link(name: &str) -> eyre::Result<()> {
    let ifname_attr = RtattrBuilder::default()
        .rta_type(Ifla::Ifname)
        .rta_payload(name)
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;

    let kind_attr = RtattrBuilder::default()
        .rta_type(IflaInfo::Kind)
        .rta_payload("vcan")
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;
    let linkinfo_attr = RtattrBuilder::default()
        .rta_type(Ifla::Linkinfo)
        .rta_payload(Vec::<u8>::new())
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?
        .nest(&kind_attr)?;

    let mut attrs = RtBuffer::new();
    attrs.push(ifname_attr);
    attrs.push(linkinfo_attr);

    let msg = IfinfomsgBuilder::default()
        .ifi_family(RtAddrFamily::Unspecified)
        .rtattrs(attrs)
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;

    send(Rtm::Newlink, NlmF::CREATE | NlmF::EXCL | NlmF::ACK, msg)
}

/// Bring a network interface up.
pub fn set_link_up(name: &str) -> eyre::Result<()> {
    let index = resolve_ifindex(name)?;
    let msg = IfinfomsgBuilder::default()
        .ifi_family(RtAddrFamily::Unspecified)
        .ifi_index(index)
        .up()
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;
    send(Rtm::Newlink, NlmF::ACK, msg)
}

/// Bring a network interface down.
pub fn set_link_down(name: &str) -> eyre::Result<()> {
    let index = resolve_ifindex(name)?;
    let msg = IfinfomsgBuilder::default()
        .ifi_family(RtAddrFamily::Unspecified)
        .ifi_index(index)
        .down()
        .build()
        .map_err(|e| eyre::eyre!("{e}"))?;
    send(Rtm::Newlink, NlmF::ACK, msg)
}

fn resolve_ifindex(name: &str) -> eyre::Result<i32> {
    let name_c = CString::new(name)?;
    let index = unsafe { libc::if_nametoindex(name_c.as_ptr()) };
    if index == 0 {
        bail!("interface not found: {name:?}");
    }
    Ok(index as i32)
}

/// Send a netlink message and drain the response.
fn send(msg_type: Rtm, flags: NlmF, msg: Ifinfomsg) -> eyre::Result<()> {
    let (rtnl, _) = NlRouter::connect(NlFamily::Route, None, Groups::empty())?;
    let recv: Vec<_> = rtnl
        .send::<_, _, Rtm, Ifinfomsg>(msg_type, flags, NlPayload::Payload(msg))?
        .collect();
    // NlRouter returns errors as Err items in the iterator. Collecting drains them, and the
    // send() call itself returns Err if the kernel responds with a netlink error.
    for res in recv {
        res?;
    }
    Ok(())
}
