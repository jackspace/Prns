package org.personal.prns.controller

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiManager
import android.os.Build
import android.util.Log
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.util.Collections

class WifiLanLink(context: Context) {
    private val appContext = context.applicationContext
    private val connectivity =
        appContext.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
    private val multicastLock =
        (appContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager)
            ?.createMulticastLock("PrnsControllerAutoWifi")
            ?.apply { setReferenceCounted(false) }

    @Volatile
    private var running = false
    private var worker: Thread? = null
    private var lastPublished: String? = null

    fun start() {
        if (running) {
            return
        }
        running = true
        worker = Thread(::runPump, "prns-wifi-lan").also { it.start() }
    }

    fun stop() {
        running = false
        NativeBridge.nativeWifiLanWaitForWork(0)
        worker?.join(WORKER_JOIN_MILLIS)
        worker = null
        releaseMulticastLock()
    }

    private fun runPump() {
        while (running) {
            publishInterfaces()
            applyMulticastLock(NativeBridge.nativeWifiLanShouldHoldMulticastLock())
            NativeBridge.nativeWifiLanWaitForWork(REFRESH_MILLIS)
        }
        releaseMulticastLock()
    }

    private fun publishInterfaces() {
        val snapshots = collectLanInterfaces()
        if (snapshots.isEmpty()) {
            NativeBridge.nativeWifiLanSetInterfaces(
                emptyArray(),
                IntArray(0),
                emptyArray(),
                emptyArray(),
            )
            return
        }
        NativeBridge.nativeWifiLanSetInterfaces(
            snapshots.map { it.name }.toTypedArray(),
            snapshots.map { it.index }.toIntArray(),
            snapshots.map { it.addressOctets }.toTypedArray(),
            snapshots.map { it.prefixLengths }.toTypedArray(),
        )
        val published = snapshots.joinToString { "${it.name}%${it.index}" }
        if (published != lastPublished) {
            lastPublished = published
            Log.i(TAG, "lan interfaces $published")
        }
    }

    private fun collectLanInterfaces(): List<LanInterfaceSnapshot> {
        val byIndex = LinkedHashMap<Int, LanInterfaceSnapshot>()
        addConnectivityManagerInterfaces(byIndex)
        addNetworkInterfaceFallbacks(byIndex)
        return byIndex.values.toList()
    }

    private fun addConnectivityManagerInterfaces(byIndex: MutableMap<Int, LanInterfaceSnapshot>) {
        val manager = connectivity ?: return
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.LOLLIPOP) {
            return
        }
        for (network in manager.allNetworks) {
            addNetwork(manager, network, byIndex)
        }
    }

    private fun addNetwork(
        manager: ConnectivityManager,
        network: Network,
        byIndex: MutableMap<Int, LanInterfaceSnapshot>,
    ) {
        val capabilities = manager.getNetworkCapabilities(network) ?: return
        if (!capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) &&
            !capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)
        ) {
            return
        }
        val properties = manager.getLinkProperties(network) ?: return
        val snapshot = snapshotFromLinkProperties(properties) ?: return
        byIndex.putIfAbsent(snapshot.index, snapshot)
    }

    private fun snapshotFromLinkProperties(properties: LinkProperties): LanInterfaceSnapshot? {
        val name = properties.interfaceName ?: return null
        if (name.isEmpty() || isVirtualLanName(name)) {
            return null
        }
        val index = interfaceIndex(name) ?: return null
        val addresses = ArrayList<ByteArray>()
        val prefixes = ArrayList<Int>()
        for (linkAddress in properties.linkAddresses) {
            val inet = linkAddress.address
            val octets = addressOctets(inet) ?: continue
            addresses.add(octets)
            prefixes.add(linkAddress.prefixLength)
        }
        if (addresses.isEmpty()) {
            return null
        }
        return LanInterfaceSnapshot(
            name = name,
            index = index,
            addressOctets = addresses.toTypedArray(),
            prefixLengths = prefixes.toIntArray(),
        )
    }

    private fun addNetworkInterfaceFallbacks(byIndex: MutableMap<Int, LanInterfaceSnapshot>) {
        try {
            for (networkInterface in Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!networkInterface.isUp
                    || networkInterface.isLoopback
                    || networkInterface.index <= 0
                    || isVirtualLanName(networkInterface.name)
                ) {
                    continue
                }
                if (byIndex.containsKey(networkInterface.index)) {
                    continue
                }
                val addresses = ArrayList<ByteArray>()
                val prefixes = ArrayList<Int>()
                for (inet in Collections.list(networkInterface.inetAddresses)) {
                    val octets = addressOctets(inet) ?: continue
                    addresses.add(octets)
                    prefixes.add(defaultPrefixLength(inet))
                }
                if (addresses.isEmpty()) {
                    continue
                }
                byIndex[networkInterface.index] = LanInterfaceSnapshot(
                    name = networkInterface.name,
                    index = networkInterface.index,
                    addressOctets = addresses.toTypedArray(),
                    prefixLengths = prefixes.toIntArray(),
                )
            }
        } catch (failure: Exception) {
            Log.d(TAG, "NetworkInterface scan unavailable", failure)
        }
    }

    private fun isVirtualLanName(name: String): Boolean {
        return VIRTUAL_LAN_PREFIXES.any { prefix -> name.startsWith(prefix) }
    }

    private fun interfaceIndex(name: String): Int? {
        return try {
            NetworkInterface.getByName(name)
                ?.takeIf { networkInterface -> networkInterface.index > 0 }
                ?.index
        } catch (failure: Exception) {
            Log.d(TAG, "ifindex unavailable for $name", failure)
            null
        }
    }

    private fun addressOctets(address: InetAddress): ByteArray? {
        return when (address) {
            is Inet4Address -> {
                if (address.isLoopbackAddress || address.isMulticastAddress) {
                    null
                } else {
                    address.address
                }
            }
            is Inet6Address -> {
                if (address.isLoopbackAddress || address.isMulticastAddress) {
                    null
                } else {
                    address.address
                }
            }
            else -> null
        }
    }

    private fun defaultPrefixLength(address: InetAddress): Int {
        return when (address) {
            is Inet4Address -> 24
            is Inet6Address -> 64
            else -> 0
        }
    }

    private fun applyMulticastLock(shouldHold: Boolean) {
        if (shouldHold) {
            acquireMulticastLock()
        } else {
            releaseMulticastLock()
        }
    }

    private fun acquireMulticastLock() {
        val lock = multicastLock ?: return
        if (lock.isHeld) {
            return
        }
        try {
            lock.acquire()
            Log.i(TAG, "wifi multicast lock acquired")
        } catch (failure: RuntimeException) {
            Log.w(TAG, "wifi multicast lock unavailable", failure)
        }
    }

    private fun releaseMulticastLock() {
        val lock = multicastLock ?: return
        if (!lock.isHeld) {
            return
        }
        try {
            lock.release()
            Log.i(TAG, "wifi multicast lock released")
        } catch (failure: RuntimeException) {
            Log.w(TAG, "could not release wifi multicast lock", failure)
        }
    }

    private data class LanInterfaceSnapshot(
        val name: String,
        val index: Int,
        val addressOctets: Array<ByteArray>,
        val prefixLengths: IntArray,
    )

    private companion object {
        private const val TAG = "PrnsWifiLan"
        private const val REFRESH_MILLIS = 1_000L
        private const val WORKER_JOIN_MILLIS = 2_000L
        private val VIRTUAL_LAN_PREFIXES = arrayOf("dummy")
    }
}
