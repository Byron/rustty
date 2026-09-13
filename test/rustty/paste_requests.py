"""State-aware paste behavior against the native terminal paste entry point."""
import base64


def requests():
    def content(mime, data):
        return {"mime": mime.hex(), "data": data.hex()}

    def write(data):
        return {"op": "write", "data": data.hex()}

    def paste(**options):
        return {"op": "paste", "paste": options}

    def case(name, operations, **options):
        return ({"id": "protocol/paste/" + name, "operations": operations, **options},
                ["clipboard", "input.focus-paste", "effects.pty"])

    payloads = [[], [content(b"image/png", b"\x89PNG")]]
    payloads += [[content(b"text/plain", data)] for data in
                 (b"", b"hello", b"a\nb", b"a\r\nb", b"\xff\0\xfe", b"a\x1b[201~b")]
    payloads += [[content(b"image/png", b"large image"), content(b"TEXT", b"first"), content(b"text/plain", b"later")],
                 [content(b"text/plain", b""), content(b"TEXT", b"ignored")]]
    for source in ("text", "standard", "primary", "selection"):
        for kitty in (False, True):
            for bracketed in (False, True):
                modes = (b"\x1b[?5522h" if kitty else b"") + (b"\x1b[?2004h" if bracketed else b"")
                for callback in (False, True):
                    for reader in (False, True):
                        for allow in (False, True):
                            for index, contents in enumerate(payloads):
                                name = f"matrix/{source}/{kitty}/{bracketed}/{callback}/{reader}/{allow}/{index}"
                                yield case(name, [write(modes), paste(source=source, contents=contents,
                                           reader=reader, allow_unsafe=allow)], clipboard_read_enabled=callback)

    for mime in (b"text/plain", b"text/plain;charset=utf-8", b"UTF8_STRING", b"TEXT", b"STRING", b"text/html"):
        for reader in (False, True):
            yield case(f"mime/{mime.hex()}/{reader}", [paste(contents=[content(mime, b"text")], reader=reader)])
    for size in (4095, 4096, 4097, 8193):
        for bracketed in (False, True):
            yield case(f"chunks/{size}/{bracketed}", [write(b"\x1b[?2004h" if bracketed else b""),
                paste(contents=[content(b"text/plain", bytes(i % 256 for i in range(size)))], allow_unsafe=True, reader=True)])

    for kitty in (False, True):
        for reader in (False, True):
            for callback in (False, True):
                for failure in ("read_error", "entropy_error"):
                    yield case(f"failure/{kitty}/{reader}/{callback}/{failure}",
                        [write(b"\x1b[?5522h" if kitty else b""),
                         paste(contents=[content(b"text/plain", b"test")], reader=reader, **{failure: True})],
                        clipboard_read_enabled=callback)

    for count in (0, 1, 15, 16, 17, 32):
        yield case(f"listing/count/{count}", [write(b"\x1b[?5522h"),
            paste(contents=[content(f"mime/{i}".encode(), b"unread") for i in range(count)], reader=True, read_error=True)])
    for size in (4093, 4094, 4095, 4096, 4097):
        yield case(f"listing/size/{size}", [write(b"\x1b[?5522h"), paste(contents=[content(b"x" * size, b""), content(b"y", b"")])])
    for index, entropy in enumerate((bytes(range(256)), b"\xff\xff\0", b"\xdf\xe0\xff\x01", b"\xff\xfe\xfd\x02")):
        yield case(f"entropy/{index}", [write(b"\x1b[?5522h"), paste(entropy=entropy.hex())])

    password = b"2" * 22  # Native rejection sampler with deterministic zero entropy.
    credentials = b":name=" + base64.b64encode(b"program") + b":pw=" + base64.b64encode(password)
    def read(mime=b"text/plain", creds=credentials):
        return write(b"\x1b]5522;type=read" + creds + b";" + base64.b64encode(mime) + b"\x1b\\")
    event = paste(contents=[content(b"text/plain", b"never read here")], reader=True, read_error=True)
    begin = write(b"\x1b[?5522h")
    scenarios = {
        "once": [read(), read()],
        "listing-preserves": [read(b"."), read(), read()],
        "different-name": [read(creds=credentials.replace(base64.b64encode(b"program"), base64.b64encode(b"other"))), read()],
        "missing-name-preserves": [read(creds=b":pw=" + base64.b64encode(password)), read(), read()],
        "wrong-direction-consumes": [write(b"\x1b]5522;type=write" + credentials + b"\x07"),
                                     write(b"\x1b]5522;type=wdata\x07"), read()],
        "reset-clears": [write(b"\x1bc"), read()],
        "direct-reset-preserves": [{"op": "terminal_reset"}, read(), read()],
        "failed-event-no-grant": [paste(entropy_error=True), read()],
    }
    for name, operations in scenarios.items():
        yield case("grants/" + name, [begin, *([] if name == "failed-event-no-grant" else [event]), *operations])
