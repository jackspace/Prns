package dev.dioxus.main

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import org.personal.prns.controller.BleLink
import org.personal.prns.controller.UsbLink
import org.personal.prns.controller.WifiLanLink

typealias BuildConfig = org.personal.prns.controller.BuildConfig

class MainActivity : WryActivity() {
    private var bleLink: BleLink? = null
    private var usbLink: UsbLink? = null
    private var wifiLanLink: WifiLanLink? = null
    private var lastPermissionState: List<Boolean>? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestMissingPermissions()
        startLinks()
    }

    override fun onResume() {
        super.onResume()
        val current = permissionState()
        if (lastPermissionState != null && lastPermissionState != current) {
            startLinks()
        }
        lastPermissionState = current
    }

    override fun onDestroy() {
        runCatching { bleLink?.stop() }
        runCatching { usbLink?.stop() }
        runCatching { wifiLanLink?.stop() }
        bleLink = null
        usbLink = null
        wifiLanLink = null
        super.onDestroy()
    }

    private fun startLinks() {
        if (usbLink == null) {
            usbLink = runCatching { UsbLink(applicationContext).also { it.start() } }
                .onFailure { Log.e(TAG, "USB Auto link failed to start", it) }
                .getOrNull()
        }
        if (wifiLanLink == null) {
            wifiLanLink = runCatching { WifiLanLink(applicationContext).also { it.start() } }
                .onFailure { Log.e(TAG, "Wi-Fi LAN link failed to start", it) }
                .getOrNull()
        }
        if (bleLink == null && hasBlePermissions()) {
            bleLink = runCatching { BleLink(applicationContext).also { it.start() } }
                .onFailure { Log.e(TAG, "Bluetooth LE Auto link failed to start", it) }
                .getOrNull()
        }
    }

    private fun requestMissingPermissions() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) {
            return
        }
        val needed = runtimePermissions().filter { permission ->
            checkSelfPermission(permission) != PackageManager.PERMISSION_GRANTED
        }
        if (needed.isNotEmpty()) {
            requestPermissions(needed.toTypedArray(), PERMISSION_REQUEST)
        }
    }

    private fun permissionState(): List<Boolean> =
        runtimePermissions().map { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    private fun runtimePermissions(): List<String> {
        val permissions = mutableListOf<String>()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            permissions += Manifest.permission.NEARBY_WIFI_DEVICES
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            permissions += listOf(
                Manifest.permission.BLUETOOTH_SCAN,
                Manifest.permission.BLUETOOTH_ADVERTISE,
                Manifest.permission.BLUETOOTH_CONNECT,
            )
        }
        if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.S_V2) {
            permissions += listOf(
                Manifest.permission.ACCESS_COARSE_LOCATION,
                Manifest.permission.ACCESS_FINE_LOCATION,
            )
        }
        return permissions
    }

    private fun hasBlePermissions(): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            return false
        }
        return runtimePermissions()
            .filter { it.startsWith("android.permission.BLUETOOTH") || it.contains("LOCATION") }
            .all { checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }
    }

    companion object {
        private const val TAG = "PrnsController"
        private const val PERMISSION_REQUEST = 0x5052
    }
}
