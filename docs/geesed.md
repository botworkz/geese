# geesed v0

`geesed` v0 is deliberately small: it boots, listens on a control socket, answers a `status` ping, and shuts down cleanly. Profile CRUD and ACP routing land in later issues; see phlax/geese#10 for the sequence.

`geese status` can be pointed at a non-default daemon socket by setting `GEESED_SOCKET`.
