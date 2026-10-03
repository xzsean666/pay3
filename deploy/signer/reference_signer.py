#!/usr/bin/env python3
"""
Pay3 Reference HTTP Signer Service

This reference signer implements the Pay3 Remote Signer HTTP API contract:
  - GET  /healthz
  - POST /v1/addresses/derive
  - POST /v1/transactions/sign

It supports Bearer token authentication and can be used as a standalone
service in staging/test environments, or adapted to integrate with Cloud KMS
(AWS KMS, Google Cloud KMS, HashiCorp Vault, Azure Key Vault).

Environment Variables:
  - SIGNER_PORT: Port to listen on (default: 8088)
  - SIGNER_HOST: Host to bind to (default: 0.0.0.0)
  - SIGNER_AUTH_TOKEN: Bearer token required for authentication (optional, recommended)
  - SIGNER_KEY_REF: Expected key_ref name (default: pay3-master)
"""

import json
import os
import sys
import hashlib
from http.server import HTTPServer, BaseHTTPRequestHandler

HOST = os.getenv("SIGNER_HOST", "0.0.0.0")
PORT = int(os.getenv("SIGNER_PORT", "8088"))
AUTH_TOKEN = os.getenv("SIGNER_AUTH_TOKEN", "")
EXPECTED_KEY_REF = os.getenv("SIGNER_KEY_REF", "pay3-master")


def derive_deterministic_address(key_ref: str, path: str) -> str:
    """Deterministic mock derivation matching test fixtures."""
    h = hashlib.sha256(f"pay3-ref-signer:{key_ref}:{path}".encode()).digest()
    addr_bytes = h[-20:]
    return "0x" + addr_bytes.hex()


def sign_transaction_mock(key_ref: str, path: str, tx: dict) -> dict:
    """Deterministic mock signing matching test fixtures."""
    request_id = tx.get("request_id", "")
    chain_id = tx.get("chain_id", 1)
    nonce = tx.get("nonce", 0)
    from_addr = derive_deterministic_address(key_ref, path)

    payload = f"{request_id}:{chain_id}:{nonce}:{from_addr}:{tx.get('to', '')}".encode()
    tx_hash_bytes = hashlib.sha256(payload).digest()
    tx_hash = "0x" + tx_hash_bytes.hex()

    # Raw tx dummy bytes starting with EIP-1559 type byte 0x02
    raw_tx = [2] + list(tx_hash_bytes)

    return {
        "request_id": request_id,
        "chain_id": chain_id,
        "nonce": nonce,
        "from": from_addr,
        "to": tx.get("to"),
        "raw_tx": raw_tx,
        "tx_hash": tx_hash,
    }


def sign_digest_mock(key_ref: str, path: str, digest: str) -> dict:
    """Deterministic mock typed data signature matching test fixtures."""
    r_bytes = hashlib.sha256(f"pay3-digest-r:{key_ref}:{path}:{digest}".encode()).digest()
    s_bytes = hashlib.sha256(f"pay3-digest-s:{key_ref}:{path}:{digest}".encode()).digest()
    return {
        "v": 27,
        "r": "0x" + r_bytes.hex(),
        "s": "0x" + s_bytes.hex(),
    }


class SignerHandler(BaseHTTPRequestHandler):
    def _send_json(self, status: int, data: any):
        body = json.dumps(data).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_error(self, status: int, message: str):
        self._send_json(status, {"error": message})

    def _check_auth(self) -> bool:
        if not AUTH_TOKEN:
            return True
        auth_header = self.headers.get("Authorization", "")
        expected = f"Bearer {AUTH_TOKEN}"
        if auth_header != expected:
            self._send_error(401, "Unauthorized: missing or invalid Bearer token")
            return False
        return True

    def do_GET(self):
        if self.path == "/healthz":
            if not self._check_auth():
                return
            self._send_json(200, {"status": "ok"})
        else:
            self._send_error(404, "Not Found")

    def do_POST(self):
        if not self._check_auth():
            return

        content_length = int(self.headers.get("Content-Length", 0))
        if content_length == 0:
            self._send_error(400, "Empty request body")
            return

        body = self.rfile.read(content_length)
        try:
            req = json.loads(body.decode("utf-8"))
        except Exception as e:
            self._send_error(400, f"Malformed JSON: {e}")
            return

        if self.path == "/v1/addresses/derive":
            key_ref = req.get("key_ref")
            path = req.get("path")
            if not key_ref or not path:
                self._send_error(400, "Missing key_ref or path")
                return
            if EXPECTED_KEY_REF and key_ref != EXPECTED_KEY_REF:
                self._send_error(400, f"Unknown key_ref: {key_ref}")
                return
            addr = derive_deterministic_address(key_ref, path)
            self._send_json(200, addr)

        elif self.path == "/v1/transactions/sign":
            key_ref = req.get("key_ref")
            path = req.get("path")
            tx = req.get("transaction")
            if not key_ref or not path or not tx:
                self._send_error(400, "Missing key_ref, path, or transaction")
                return
            if EXPECTED_KEY_REF and key_ref != EXPECTED_KEY_REF:
                self._send_error(400, f"Unknown key_ref: {key_ref}")
                return
            signed_tx = sign_transaction_mock(key_ref, path, tx)
            self._send_json(200, signed_tx)

        elif self.path == "/v1/digests/sign":
            key_ref = req.get("key_ref")
            path = req.get("path")
            digest = req.get("digest")
            if not key_ref or not path or not digest:
                self._send_error(400, "Missing key_ref, path, or digest")
                return
            if EXPECTED_KEY_REF and key_ref != EXPECTED_KEY_REF:
                self._send_error(400, f"Unknown key_ref: {key_ref}")
                return
            sig = sign_digest_mock(key_ref, path, digest)
            self._send_json(200, sig)

        else:
            self._send_error(404, "Not Found")

    def log_message(self, format, *args):
        # Suppress noisy default logging
        sys.stderr.write(f"[signer] {self.address_string()} - {format % args}\n")


def run():
    server = HTTPServer((HOST, PORT), SignerHandler)
    sys.stderr.write(f"Pay3 Reference Signer running on http://{HOST}:{PORT}\n")
    if AUTH_TOKEN:
        sys.stderr.write("Bearer authentication enabled\n")
    else:
        sys.stderr.write("WARNING: No SIGNER_AUTH_TOKEN configured\n")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    server.server_close()


if __name__ == "__main__":
    run()
