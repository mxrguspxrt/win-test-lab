#!/usr/bin/env python3
"""msdl.py - resolve a Microsoft consumer Windows ISO download link (Win10/Win11).

    msdl.py SKU                 print the ISO download URL(s) for a SKU id
    msdl.py skus PRODUCT_ID     list English SKUs for a product edition id

Uses the same public software-download-connector API the microsoft.com page uses. The link
is tied to the session id and expires after ~24h. Network only; writes nothing.
Product edition ids: Win11 24H2 = 3113, Win10 22H2 = 2618 (may change over time).
"""
import json
import sys
import urllib.request
import uuid

BASE = "https://www.microsoft.com/software-download-connector/api"
UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/120.0 Safari/537.36")


def get(url, referer):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Referer": referer})
    with urllib.request.urlopen(req, timeout=40) as r:
        return json.load(r)


def main(argv):
    sid = str(uuid.uuid4())
    # the session must be "primed" against the fp endpoint first
    try:
        urllib.request.urlopen(urllib.request.Request(
            "https://vlscppe.microsoft.com/fp/tags?org_id=y6jn8c31&session_id=" + sid,
            headers={"User-Agent": UA}), timeout=30).read()
    except Exception:  # noqa: BLE001
        pass

    if len(argv) == 3 and argv[1] == "skus":
        pid = argv[2]
        url = ("%s/getskuinformationbyproductedition?profile=606624d44113&ProductEditionId=%s"
               "&SKU=&friendlyFileName=&Locale=en-US&sessionID=%s" % (BASE, pid, sid))
        d = get(url, "https://www.microsoft.com/software-download/windows11")
        for s in d.get("Skus", []):
            if "English" in s.get("Language", ""):
                print("%s\t%s" % (s["Id"], s["LocalizedProductDisplayName"]))
        return 0

    if len(argv) == 2:
        sku = argv[1]
        url = ("%s/GetProductDownloadLinksBySku?profile=606624d44113&productEditionId=undefined"
               "&SKU=%s&friendlyFileName=undefined&Locale=en-US&sessionID=%s" % (BASE, sku, sid))
        d = get(url, "https://www.microsoft.com/software-download/windows11")
        errs = d.get("ValidationContainer", {}).get("Errors", [])
        if errs:
            sys.stderr.write("microsoft API error: %s\n" % json.dumps(errs))
            return 2
        opts = d.get("ProductDownloadOptions", [])
        if not opts:
            sys.stderr.write("no download options returned (link may be rate-limited; try again)\n")
            return 2
        for o in opts:
            print(o.get("Uri", ""))
        return 0

    sys.stderr.write(__doc__)
    return 64


if __name__ == "__main__":
    sys.exit(main(sys.argv))
