package org.personal.prns.controller

import java.nio.ByteBuffer

object NativeBridge {
    const val BLE_RADIO_ENABLED = 0x01
    const val BLE_RADIO_ADVERTISING = 0x02
    const val BLE_RADIO_SCANNING = 0x04
    const val BLE_INGRESS_ACCEPTED = 0
    const val BLE_INGRESS_FULL = 1
    const val BLE_INGRESS_CLOSED = 2

    init {
        System.loadLibrary("main")
    }

    external fun nativeUsbConnected(connected: Boolean)

    external fun nativeUsbAutoVendorId(): Int

    external fun nativeUsbAutoProductId(): Int

    external fun nativeUsbAccessoryManufacturer(): String

    external fun nativeUsbAccessoryModel(): String

    external fun nativeUsbAccessoryDescription(): String

    external fun nativeUsbAccessoryVersion(): String

    external fun nativeUsbAccessoryUri(): String

    external fun nativeUsbAccessorySerial(): String

    external fun nativeUsbRx(buffer: ByteBuffer, len: Int)

    external fun nativeUsbTx(buffer: ByteBuffer): Int

    external fun nativeBleSetPsm(psm: Int)

    external fun nativeBleDesiredState(): Int

    external fun nativeBlePeerCapacity(): Int

    external fun nativeBleWorkGeneration(): Long

    external fun nativeBleWaitForWork(observed: Long, timeoutMillis: Long): Long

    external fun nativeBleWakePumps()

    external fun nativeBleIdentity(buffer: ByteBuffer): Int

    external fun nativeBleSighting(address: ByteBuffer, rssi: Int)

    /** `1` dial, `0` accept, `-1` keep the v4 C′ path. */
    external fun nativeBleTypedDialAction(rolePayload: ByteBuffer): Int

    external fun nativeBleDialFailed(address: ByteBuffer): Boolean

    external fun nativeBleLinkUp(connId: Int, address: ByteBuffer, rssi: Int, dialed: Boolean): Boolean

    external fun nativeBleColumbaLinkUp(
        connId: Int,
        address: ByteBuffer,
        rssi: Int,
        dialed: Boolean,
        peerIdentity: ByteBuffer,
    ): Boolean

    external fun nativeBleControlIn(connId: Int, buffer: ByteBuffer, len: Int): Int

    external fun nativeBleControlOut(connId: Int, buffer: ByteBuffer): Int

    external fun nativeBleCommitControlOut(connId: Int): Boolean

    external fun nativeBleL2capIn(connId: Int, buffer: ByteBuffer, len: Int): Boolean

    external fun nativeBleL2capOut(connId: Int, buffer: ByteBuffer): Int

    external fun nativeBleDataIn(connId: Int, buffer: ByteBuffer, len: Int): Int

    external fun nativeBleDataOut(connId: Int, buffer: ByteBuffer): Int

    external fun nativeBleCommitDataOut(connId: Int): Boolean

    external fun nativeBleL2capUp(connId: Int)

    external fun nativeBleDisconnected(connId: Int)

    external fun nativeBleNextClose(): Int

    external fun nativeBleNextDial(buffer: ByteBuffer): Boolean

    external fun nativeBleNextL2capOpen(buffer: ByteBuffer): Boolean

    external fun nativeWifiLanShouldHoldMulticastLock(): Boolean

    external fun nativeWifiLanWaitForWork(timeoutMillis: Long)

    external fun nativeWifiLanSetInterfaces(
        names: Array<String>,
        indexes: IntArray,
        addresses: Array<Array<ByteArray>>,
        prefixLengths: Array<IntArray>,
    ): Boolean
}
