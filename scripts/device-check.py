#!/usr/bin/env python3
"""Warns about an Android device's network setup that hurts streaming from this Mac.

Usage: device-check.py CONNECTIVITY_DUMP MAC_IP

CONNECTIVITY_DUMP is the output of `adb shell dumpsys connectivity`. The script prints one
warning per line, or nothing:

- a VPN on the device routes the Mac's address. The device's own UDP then goes through the
  tunnel; with a Tailscale subnet route the subnet router changes its sender address, and ALVR
  drops every stream packet;
- the device uses 2.4 GHz Wi-Fi;
- the Wi-Fi link is slow.
"""

import ipaddress
import re
import sys

# Below this link speed in Mbps, a 100 Mbps stream has no room for resends
SLOW_LINK_MBPS = 400


def networks(dump):
    """The text of each network the device has, by its NetworkAgentInfo line."""
    return [line for line in dump.splitlines() if "NetworkAgentInfo{" in line]


def vpn_routes(info):
    interface = re.search(r"InterfaceName: (\S+)", info)
    mtu = re.search(r"MTU: (\d+)", info)
    routes = re.findall(r"(\d+\.\d+\.\d+\.\d+/\d+) -> \S+ (\S+)", info)
    return (interface.group(1) if interface else "?"), (mtu.group(1) if mtu else "?"), routes


def main():
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    dump = open(sys.argv[1], encoding="utf-8", errors="replace").read()
    mac = ipaddress.ip_address(sys.argv[2])
    for info in networks(dump):
        if "VPN CONNECTED" in info:
            interface, mtu, routes = vpn_routes(info)
            covering = [prefix for prefix, _ in routes if mac in ipaddress.ip_network(prefix, strict=False)]
            if covering:
                print(
                    f"A VPN on the device ({interface}, MTU {mtu}) routes this Mac's address {mac} "
                    f"through {covering[0]}: the device's UDP takes the tunnel. Pause the VPN "
                    "(--pause-tailscale), exclude the app from it, or stream over TCP."
                )
        elif "WIFI CONNECTED" in info:
            frequency = re.search(r"Frequency: (\d+)MHz", info)
            if frequency and int(frequency.group(1)) < 3000:
                print(f"The device uses 2.4 GHz Wi-Fi ({frequency.group(1)} MHz). Use 5 or 6 GHz for VR streaming.")
            speed = re.search(r"Link speed: (\d+)Mbps", info)
            if speed and int(speed.group(1)) < SLOW_LINK_MBPS:
                print(f"The device's Wi-Fi link runs at {speed.group(1)} Mbps.")


if __name__ == "__main__":
    main()
