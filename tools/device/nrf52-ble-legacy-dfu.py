"""Nordic LEGACY DFU (SDK 11 style, what Adafruit_nRF52_Bootloader speaks) over BLE, with bleak.

Usage:
  python nrf52-ble-legacy-dfu.py <package.zip> [--address AA:BB:CC:DD:EE:FF] [--prn 8]
  python nrf52-ble-legacy-dfu.py --finish --address AA:BB:CC:DD:EE:FF
  python nrf52-ble-legacy-dfu.py --reset --address AA:BB:CC:DD:EE:FF

Mirrors nrfutil 0.5 dfu_transport_ble.py: START_DFU(app) + sizes, init packet from the zip's .dat,
PRN request, RECEIVE_FIRMWARE_IMAGE with 20-byte packets, VALIDATE, ACTIVATE_AND_RESET.

--finish sends VALIDATE then ACTIVATE_AND_RESET, for a transfer whose image was fully received.
--reset sends SYS_RESET, for a bootloader left in OTA mode by an aborted transfer (it has no
timeout of its own).

Measured from one Windows host (MediaTek MT7921) against the stock XIAO bootloader 0.9.2, 603 KB
image: --prn 1 took about 62 minutes, --prn 4 about 16, --prn 8 about 8; --prn 10 failed both times
tried (10 03 06, once at 16 KB, once on the first packets). Phones pace per connection interval and
may cope with more; not measured.

START_DFU erases before it answers, which is why its response wait is 150 s. It erases only the
pages the new image needs (a CURRENT.UF2 readback after a failed transfer showed the erase ending
exactly at the image's size), so pages beyond the image, such as the remote-control identity at
0xE9000 on the Solar Node, survive. The application itself is gone until a transfer completes.
"""
import argparse, asyncio, json, struct, sys, time, zipfile
from bleak import BleakClient, BleakScanner

SVC = "00001530-1212-efde-1523-785feabcd123"
CP = "00001531-1212-efde-1523-785feabcd123"   # control point: write + notify
PKT = "00001532-1212-efde-1523-785feabcd123"  # packet: write without response
VER = "00001534-1212-efde-1523-785feabcd123"  # version: read

OP_START_DFU, OP_INIT_PARAMS, OP_RECEIVE_IMAGE, OP_VALIDATE, OP_ACTIVATE, OP_SYS_RESET, OP_PRN_REQ = 1, 2, 3, 4, 5, 6, 8
OP_RESPONSE, OP_PRN = 0x10, 0x11
IMAGE_APPLICATION = 0x04
PACKET_BYTES = 20


def ts():
    return time.strftime("%H:%M:%S")


def load_package(path):
    with zipfile.ZipFile(path) as z:
        manifest = json.loads(z.read("manifest.json"))["manifest"]
        app = manifest["application"]
        return z.read(app["bin_file"]), z.read(app["dat_file"])


async def finish(address):
    q = asyncio.Queue()
    async with BleakClient(address, timeout=20) as c:
        print(ts(), "connected")
        await c.start_notify(CP, lambda _, d: (print(ts(), "CP:", bytes(d).hex()), q.put_nowait(bytes(d))))
        await c.write_gatt_char(CP, bytes([OP_VALIDATE]), response=True)
        r = await asyncio.wait_for(q.get(), 60)
        print(ts(), "VALIDATE response:", r.hex(), "(10 04 01 = ok, 10 04 02 = invalid state, 10 04 05 = crc error)")
        if r != bytes([OP_RESPONSE, OP_VALIDATE, 0x01]):
            sys.exit(2)
        try:
            await c.write_gatt_char(CP, bytes([OP_ACTIVATE]), response=True)
        except Exception as e:  # noqa: BLE001
            print(ts(), "activate ended the connection (expected):", e)
        print(ts(), "ACTIVATE_AND_RESET sent")


async def reset(address):
    async with BleakClient(address, timeout=20) as c:
        print(ts(), "connected; sending SYS_RESET")
        try:
            await c.write_gatt_char(CP, bytes([OP_SYS_RESET]), response=True)
        except Exception as e:  # noqa: BLE001
            print(ts(), "write ended the connection (expected):", e)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("package", nargs="?")
    ap.add_argument("--address", default=None)
    ap.add_argument("--prn", type=int, default=8)
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--finish", action="store_true", help="VALIDATE then ACTIVATE_AND_RESET only")
    mode.add_argument("--reset", action="store_true", help="SYS_RESET a bootloader stuck in OTA mode")
    args = ap.parse_args()
    if args.finish or args.reset:
        if not args.address:
            ap.error("--finish and --reset need --address")
        await (finish if args.finish else reset)(args.address)
        return
    if not args.package:
        ap.error("a package .zip is required unless --finish or --reset is given")

    firmware, init_packet = load_package(args.package)
    print(f"{ts()} package: firmware {len(firmware)} bytes, init packet {len(init_packet)} bytes")

    address = args.address
    if address is None:
        print(f"{ts()} scanning for a DFU advertiser...")
        found = {}

        def cb(d, adv):
            if SVC in [s.lower() for s in (adv.service_uuids or [])] or "dfu" in (adv.local_name or "").lower():
                found[d.address] = (adv.local_name, adv.rssi)
        s = BleakScanner(cb)
        await s.start(); await asyncio.sleep(6); await s.stop()
        if not found:
            sys.exit("no DFU advertiser seen")
        address = max(found, key=lambda a: found[a][1])
        print(f"{ts()} using {address} {found[address]}")

    responses: asyncio.Queue = asyncio.Queue()

    def on_cp(_, data: bytearray):
        print(f"{ts()} CP notification: {bytes(data).hex()}")
        responses.put_nowait(bytes(data))

    async def expect_response(op):
        while True:
            r = await asyncio.wait_for(responses.get(), 150)
            if r[0] == OP_RESPONSE:
                if r[1] != op:
                    raise RuntimeError(f"response for op {r[1]} while waiting for {op}: {r.hex()}")
                if r[2] != 1:
                    raise RuntimeError(f"op {op} failed, status {r[2]} ({r.hex()})")
                return
            # PRN or other notifications are drained by the caller that expects them

    async with BleakClient(address, timeout=20) as c:
        print(f"{ts()} connected to {address}")
        try:
            ver = await c.read_gatt_char(VER)
            print(f"{ts()} DFU version characteristic: {ver.hex()}")
        except Exception as e:  # noqa: BLE001
            print(f"{ts()} version read skipped: {e}")
        await c.start_notify(CP, on_cp)

        await c.write_gatt_char(CP, bytes([OP_START_DFU, IMAGE_APPLICATION]), response=True)
        await c.write_gatt_char(PKT, struct.pack("<III", 0, 0, len(firmware)), response=False)
        await expect_response(OP_START_DFU)
        print(f"{ts()} START_DFU accepted")

        await c.write_gatt_char(CP, bytes([OP_INIT_PARAMS, 0x00]), response=True)
        for i in range(0, len(init_packet), PACKET_BYTES):
            await c.write_gatt_char(PKT, init_packet[i:i + PACKET_BYTES], response=False)
        await c.write_gatt_char(CP, bytes([OP_INIT_PARAMS, 0x01]), response=True)
        await expect_response(OP_INIT_PARAMS)
        print(f"{ts()} init packet accepted")

        prn = args.prn
        await c.write_gatt_char(CP, bytes([OP_PRN_REQ]) + struct.pack("<H", prn), response=True)
        await c.write_gatt_char(CP, bytes([OP_RECEIVE_IMAGE]), response=True)
        t0 = time.time()
        sent_packets = 0
        for i in range(0, len(firmware), PACKET_BYTES):
            await c.write_gatt_char(PKT, firmware[i:i + PACKET_BYTES], response=False)
            sent_packets += 1
            if prn and sent_packets % prn == 0:
                r = await asyncio.wait_for(responses.get(), 30)
                if r[0] == OP_RESPONSE and r[1] == OP_RECEIVE_IMAGE and r[2] == 1:
                    # The bootloader answers the final packet with the completion response, not a receipt.
                    responses.put_nowait(r)
                    break
                if r[0] != OP_PRN:
                    raise RuntimeError(f"expected PRN, got {r.hex()}")
                received = struct.unpack("<I", r[1:5])[0]
                if received != min(i + PACKET_BYTES, len(firmware)):
                    raise RuntimeError(f"receipt mismatch: device has {received}, sent {i + PACKET_BYTES}")
                if sent_packets % (prn * 200) == 0:
                    pct = 100 * received / len(firmware)
                    rate = received / max(time.time() - t0, 1e-6)
                    print(f"{ts()} {received}/{len(firmware)} bytes ({pct:.1f}%), {rate/1024:.1f} KiB/s")
        await expect_response(OP_RECEIVE_IMAGE)
        print(f"{ts()} image received in {time.time() - t0:.1f} s")

        await c.write_gatt_char(CP, bytes([OP_VALIDATE]), response=True)
        await expect_response(OP_VALIDATE)
        print(f"{ts()} image validated")

        try:
            await c.write_gatt_char(CP, bytes([OP_ACTIVATE]), response=True)
        except Exception as e:  # noqa: BLE001
            print(f"{ts()} activate write ended the connection (expected): {e}")
        print(f"{ts()} ACTIVATE_AND_RESET sent; the board should reboot into the new image")


asyncio.run(main())
