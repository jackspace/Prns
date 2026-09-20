use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use jni::objects::{JByteArray, JClass, JIntArray, JObjectArray, JString};
use jni::sys::{jboolean, jlong};
use jni::JNIEnv;
use personal_rns::wifi_auto::{
    HostLanAddress, HostLanInterface, HostLanInterfaceError, HostLanReplaceOutcome,
};

use super::lan_bridge;

const JNI_TRUE: jboolean = 1;
const JNI_FALSE: jboolean = 0;

#[no_mangle]
pub extern "system" fn Java_org_personal_prns_controller_NativeBridge_nativeWifiLanShouldHoldMulticastLock(
    _env: JNIEnv,
    _class: JClass,
) -> jboolean {
    if lan_bridge().should_hold_multicast_lock() {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[no_mangle]
pub extern "system" fn Java_org_personal_prns_controller_NativeBridge_nativeWifiLanWaitForWork(
    _env: JNIEnv,
    _class: JClass,
    timeout_millis: jlong,
) {
    let timeout = u64::try_from(timeout_millis.max(0)).unwrap_or(0);
    lan_bridge().wait_for_work(std::time::Duration::from_millis(timeout));
}

#[no_mangle]
pub extern "system" fn Java_org_personal_prns_controller_NativeBridge_nativeWifiLanSetInterfaces(
    mut env: JNIEnv,
    _class: JClass,
    names: JObjectArray,
    indexes: JIntArray,
    addresses: JObjectArray,
    prefix_lengths: JObjectArray,
) -> jboolean {
    match parse_host_lan_interfaces(&mut env, &names, &indexes, &addresses, &prefix_lengths) {
        Ok(interfaces) => match lan_bridge().inventory().replace(interfaces) {
            HostLanReplaceOutcome::Replaced | HostLanReplaceOutcome::Unchanged => JNI_TRUE,
            HostLanReplaceOutcome::Unavailable => JNI_FALSE,
        },
        Err(_invalid) => JNI_FALSE,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostLanJniError {
    ArrayUnavailable,
    LengthMismatch,
    InvalidInterface(HostLanInterfaceError),
}

fn parse_host_lan_interfaces(
    env: &mut JNIEnv,
    names: &JObjectArray,
    indexes: &JIntArray,
    addresses: &JObjectArray,
    prefix_lengths: &JObjectArray,
) -> Result<Vec<HostLanInterface>, HostLanJniError> {
    let name_count = array_len(env, names)?;
    let address_group_count = array_len(env, addresses)?;
    let prefix_group_count = array_len(env, prefix_lengths)?;
    let index_count = env
        .get_array_length(indexes)
        .ok()
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(HostLanJniError::ArrayUnavailable)?;
    if name_count != index_count
        || name_count != address_group_count
        || name_count != prefix_group_count
    {
        return Err(HostLanJniError::LengthMismatch);
    }

    let mut index_values = vec![0; index_count];
    env.get_int_array_region(indexes, 0, &mut index_values)
        .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?;

    let mut interfaces = Vec::with_capacity(name_count);
    for interface_slot in 0..name_count {
        let name_object = env
            .get_object_array_element(names, interface_slot as i32)
            .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?;
        let name = required_java_string(env, &JString::from(name_object))?;
        let index = u32::try_from(index_values[interface_slot]).map_err(|_invalid| {
            HostLanJniError::InvalidInterface(HostLanInterfaceError::InvalidIndex)
        })?;
        let address_group = JObjectArray::from(
            env.get_object_array_element(addresses, interface_slot as i32)
                .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?,
        );
        let prefix_group = JIntArray::from(
            env.get_object_array_element(prefix_lengths, interface_slot as i32)
                .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?,
        );
        let parsed_addresses = parse_addresses(env, &address_group, &prefix_group)?;
        match HostLanInterface::new(name, index, parsed_addresses) {
            Ok(interface) => interfaces.push(interface),
            Err(_rejected_interface) => {}
        }
    }
    Ok(interfaces)
}

fn parse_addresses(
    env: &mut JNIEnv,
    addresses: &JObjectArray,
    prefix_lengths: &JIntArray,
) -> Result<Vec<HostLanAddress>, HostLanJniError> {
    let address_count = array_len(env, addresses)?;
    let prefix_count = env
        .get_array_length(prefix_lengths)
        .ok()
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(HostLanJniError::ArrayUnavailable)?;
    if address_count != prefix_count {
        return Err(HostLanJniError::LengthMismatch);
    }
    let mut prefix_values = vec![0; prefix_count];
    env.get_int_array_region(prefix_lengths, 0, &mut prefix_values)
        .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?;

    let mut parsed = Vec::with_capacity(address_count);
    for address_slot in 0..address_count {
        let address_object = env
            .get_object_array_element(addresses, address_slot as i32)
            .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?;
        let address_array = JByteArray::from(address_object);
        let octets = env
            .convert_byte_array(&address_array)
            .map_err(|_unavailable| HostLanJniError::ArrayUnavailable)?;
        let prefix_len = u8::try_from(prefix_values[address_slot]).map_err(|_invalid| {
            HostLanJniError::InvalidInterface(HostLanInterfaceError::InvalidPrefix)
        })?;
        let addr = ip_addr(&octets).ok_or(HostLanJniError::InvalidInterface(
            HostLanInterfaceError::AddressNotLocal,
        ))?;
        match HostLanAddress::new(addr, prefix_len) {
            Ok(address) => parsed.push(address),
            Err(_rejected_address) => {}
        }
    }
    Ok(parsed)
}

fn ip_addr(octets: &[u8]) -> Option<IpAddr> {
    match octets.len() {
        4 => Some(IpAddr::V4(Ipv4Addr::new(
            octets[0], octets[1], octets[2], octets[3],
        ))),
        16 => {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(octets);
            Some(IpAddr::V6(Ipv6Addr::from(bytes)))
        }
        _ => None,
    }
}

fn array_len(env: &mut JNIEnv, array: &JObjectArray) -> Result<usize, HostLanJniError> {
    env.get_array_length(array)
        .ok()
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(HostLanJniError::ArrayUnavailable)
}

fn required_java_string(env: &mut JNIEnv, value: &JString) -> Result<String, HostLanJniError> {
    if value.is_null() {
        return Err(HostLanJniError::ArrayUnavailable);
    }
    env.get_string(value)
        .map_err(|_invalid| HostLanJniError::ArrayUnavailable)?
        .to_str()
        .map(str::to_owned)
        .map_err(|_invalid| HostLanJniError::ArrayUnavailable)
}

#[cfg(test)]
mod tests {
    use super::ip_addr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn packed_octets_become_ip_addresses() {
        assert_eq!(
            ip_addr(&[192, 168, 1, 18]),
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 18)))
        );
        assert_eq!(
            ip_addr(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
            Some(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)))
        );
        assert_eq!(ip_addr(&[1, 2, 3]), None);
    }
}
