# Gcloud (GCE-per-run) sandbox provider — operator setup

One Fabro server fans out to ephemeral GCE VMs: each run inserts a fresh VM,
reaches it over a host-key-pinned SSH session keyed by a per-run ephemeral
ed25519 key, runs the agent there, then deletes the VM. None of the setup below
lives in code — it is all out-of-band operator configuration.

## Control-plane service account (least privilege)

Create a **custom role** with exactly these permissions and bind it with an
instance name-prefix IAM condition `resource.name.startsWith("fabro-run-")` to
cap blast radius to fleet VMs. Do **not** grant `roles/compute.instanceAdmin.v1`.

| Permission | Used by |
| --- | --- |
| `compute.instances.insert` | `instances.insert` (create VM) |
| `compute.instances.delete` | `instances.delete` (teardown) |
| `compute.instances.get` | `instances.get` (IP + labels) |
| `compute.instances.setMetadata` | metadata scrub after registration |
| `compute.instances.getGuestAttributes` | **host-key pinning** (see below) |
| `compute.disks.create` | boot disk creation |
| `compute.subnetworks.use` | attach the VM to the subnetwork |
| `compute.zoneOperations.get` | poll the zonal insert/delete operation |

### Why `getGuestAttributes` is mandatory

Host-key pinning fetches the VM's freshly-generated SSH host key from the
Compute **guest attributes** endpoint after the insert operation is `DONE`, then
connects with `KnownHosts::Add` (never `Accept`). The provider reads it via
`getGuestAttributes`. If the SA lacks `compute.instances.getGuestAttributes`,
the endpoint returns **403** and `initialize()` fails fast with a diagnostic
naming the missing permission — it does **not** hang until the poll timeout, and
it does **not** silently fall back to an unpinned connection. An operator who
sees the 403 error must add this one permission, not loosen the role.

## Credentials (no key on disk)

`GcpAuth` prefers **metadata workload identity** (`GET 169.254.169.254/.../token`
with `Metadata-Flavor: Google`) when the control plane runs on GCP — no key file
exists. Off-GCP, set `FABRO_GCLOUD_SA_KEY_JSON` to the SA key JSON; it is signed
into a short-lived JWT (RS256) and exchanged for an access token in memory only,
**never written to disk**.

## Firewall / egress

The VM gets a network tag (`FABRO_GCLOUD_EGRESS_TAG`); create a VPC firewall
rule scoped to that tag for the authoritative egress control. The startup script
additionally applies host iptables (defence in depth) and an unconditional drop
of egress to `169.254.169.254` so untrusted job code can never reach the
metadata server (and the VM has no attached SA token anyway).

## Environment variables

| Var | Required | Meaning |
| --- | --- | --- |
| `FABRO_GCLOUD_PROJECT` | yes | GCP project id |
| `FABRO_GCLOUD_ZONE` | yes | e.g. `us-central1-a` |
| `FABRO_GCLOUD_SUBNETWORK` | yes | subnetwork name or full path |
| `FABRO_GCLOUD_VM_IMAGE` | yes | source image for the boot disk |
| `FABRO_GCLOUD_MACHINE_TYPE` | yes | e.g. `e2-standard-4` |
| `FABRO_GCLOUD_NAME_PREFIX` | no | defaults to `fabro-run-` |
| `FABRO_GCLOUD_SSH_USER` | no | defaults to `fabro` |
| `FABRO_GCLOUD_WORKING_DIR` | no | clone destination on the VM |
| `FABRO_GCLOUD_FABRO_PORT` | no | defaults to `32276` |
| `FABRO_GCLOUD_EGRESS_TAG` | no | VPC firewall network tag |
| `FABRO_GCLOUD_SA_KEY_JSON` | no | SA key JSON (off-GCP auth fallback) |
