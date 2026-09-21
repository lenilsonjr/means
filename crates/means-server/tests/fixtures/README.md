# Test-only cryptographic material

The PEM files in this directory support local mock servers and authentication
tests. They are public test keys, not credentials for any bank or deployed service.
The Inter certificates identify the synthetic test CA and loopback test endpoints.
The Enable Banking key signs requests verified by the local mock server.

Use these files only in tests. Production integrations need their own credentials
and certificates. New fixtures must use synthetic identities and endpoints.
