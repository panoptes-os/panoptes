# Base OS: build with Nix, ship an appliance

Status: superseded, 2026-10-27, by [decisions.md](decisions.md): Panoptes
ships as a Docker image built with Nix, not as an appliance. The source
review below still holds. This doc answers two questions. Would
enterprises adopt a product built on NixOS? Should agent containers use a
lighter base? The evidence is a source review, linked under
[Sources](#sources), and measurements on the test server, described in
[measurements.md](measurements.md).

## Recommendation

Build Panoptes OS from NixOS modules, and ship it the way Talos, Bottlerocket,
and ESXi ship: as an immutable, signed image whose security lifecycle Panoptes
owns. Buyers never administer NixOS, so they judge Panoptes' security
program, not the distro.

Keep the agent runtime independent of the base: systemd units, the
residency manager in [scheduler.md](scheduler.md), and the kiosk stack in
[desktop.md](desktop.md). An Ubuntu or RHEL host variant is then a
packaging job, which matters if a buyer requires NVIDIA's supported OS
list.

For agent containers, sharing makes them light; a smaller distro does not.
On the test server, Chromium costs about 250 MiB per agent and the
compositor about 10 MiB. The container mechanism costs 0.4 to 2.9 MiB per
instance with systemd, crun, runc, bwrap, nspawn, or Podman, and 5.9 MiB
with Docker ([scheduler.md](scheduler.md#an-agent-container-is-a-cgroup-tree)).

## Enterprises buy appliances and judge the vendor

Hypervisor and storage appliances either inherit a distro's security
program or replace it with their own:

| Product | Base | Lifecycle owner | What the vendor ships |
|---|---|---|---|
| Proxmox VE 9 | Debian 13 | Debian | [its own advisories][pve-psa] for its packages; no FIPS |
| TrueNAS SCALE | Debian | iXsystems | a [FIPS 140-3 module and STIG support][truenas] |
| Talos Linux | custom | Sidero Labs | an [SPDX SBOM per image][talos-sbom], cosign signatures, signed VEX, FIPS through the Go module |
| Bottlerocket | custom | AWS | dm-verity, an [advisory feed][bottlerocket] that Trivy consumes, FIPS 140-3 AWS-LC |
| ESXi | proprietary | Broadcom | [FIPS 140-3 cert #4815][esxi-fips] and a DISA STIG |

Panoptes on NixOS lands in the Talos row by default. Its buyers will read
Panoptes' SBOMs and advisories and never look at nixpkgs.

Regulated buyers already accept NixOS when a vendor stands behind it:

- DISA publishes an [Anduril NixOS STIG][stig], V1R2 of 2025-08-19.
- Anduril ships [NixOS on Jetson hardware][jetpack-nixos].
- TII builds its [Ghaf platform][ghaf] on NixOS.
- FlakeHub holds a [FedRAMP High authorization][flakehub-fedramp] on
  Knox's platform, with Nix itself outside the boundary.

## Upstream NixOS misses five procurement checks

| Check | Buyers expect | Upstream NixOS | Panoptes' answer |
|---|---|---|---|
| Support window | at least 5 years, per [EU CRA Art. 13(8)][cra-13]; Debian 5, Ubuntu 5 to 15, RHEL 10 to 14 | [7 months][nixos-2605]; 26.05 ends 2026-12-31 | buy an LTS stream: [Determinate Secure Packages][dsp] gives one release in four 60 months, and its [docs][dsp-docs] put 26.05 to May 2031; [CTRL-OS][ctrl-os] promises at least 5 years |
| CVE fixes | an SLA | [two named security-team members][nixos-sec]; the foundation's [2025 revenue][nixos-fin] was EUR 93,897 | the bought stream's SLA, which fixes critical CVEs within 7 days |
| Scanner coverage | Trivy, Qualys, Tenable, and Snyk see the packages | only [Wiz][wiz] lists NixOS; [OSV][osv] has no Nix ecosystem, and [purl][purl] has no `nix` type | per release, a CycloneDX SBOM and VEX from [sbomnix][sbomnix] or [bombon][bombon], and an advisory feed in a format scanners ingest |
| FIPS | a CMVP-validated module | no certificate names NixOS; the STIG's FIPS check is the [`fips=1` kernel flag][stig-fips] | build a validated provider, such as [OpenSSL 3.1.2, #4985][openssl-fips], to its security policy, and affirm the operating environment |
| GPU vendor support | NVIDIA AI Enterprise support | absent from the [support matrix][nvaie], which lists Ubuntu, RHEL, and SLES | Panoptes owns driver QA, and offers an Ubuntu LTS host variant to buyers who require NVIDIA support |

The [EU CRA][cra] reporting duties have applied since 2026-09-11, and its
main obligations apply from 2027-12-11. A product sold in the EU therefore
needs the SBOM, the advisory feed, and a 5-year support plan in its first
release.

## Lighter containers come from sharing, not from a smaller distro

Each agent on the test server is one Cage kiosk running Chromium at
1280x720. These are median memory figures for one agent, in MiB:

```text
part                                          memory.current  PSS
Cage + Chromium, about:blank                  184             431
Cage + Chromium, Greenhouse job board         254             510
  Cage itself                                                 10
  XFCE desktop in place of Cage               +95             +117
  wayvnc for a human viewer                   +11             +23
headless Chromium, same page, no compositor   250             498
```

The distro does not appear in that table. Binaries cost memory through file
pages, and every agent that maps the same file shares one copy in the page
cache. PSS charges a process for its share of every page it maps, while
memory.current leaves out pages that another cgroup first read. With one
agent running, the gap between the columns, about 250 MiB, is mostly
Chromium's own file pages. That is roughly what one browser build costs a
host, once. The density run in [desktop.md](desktop.md) confirms it: PSS
per agent fell from 510 MiB for one agent to about 237 at 64 agents.

Rules that follow:

1. Every agent of a release maps the same read-only image: the Nix store,
   or one EROFS or composefs image for OCI agents. One store path is one
   inode and one page-cache copy.
2. A second browser build on a host costs its resident file pages again.
   Pin one build per release.
3. Image size and package count do not limit density. Alpine saves disk
   and download size; it changes neither column above.

## Alternatives

| Base | Wins | Loses |
|---|---|---|
| NixOS appliance, recommended | reproducible closures; one store shared by every agent; atomic upgrades and rollback; workloads are already flake modules | Panoptes builds or buys the lifecycle evidence; NVIDIA lists it nowhere |
| Debian, the Proxmox model | Debian's security team; [5-year LTS, ELTS to 10][debian-lts]; every scanner | no FIPS; image builds and A/B updates are Panoptes' work |
| Ubuntu LTS or Core | FIPS 140-3 modules; [10 to 15 years with Pro][ubuntu-cycle]; on NVIDIA's list | Pro features are licensed per machine |
| RHEL image mode | already approved by enterprise operations teams; FIPS; [10 years][rhel-cycle] | a subscription per host; RHEL's package set |
| Alpine | small images | musl; 2-year support; no FIPS; no buyer asks for it |
| custom minimal, the Talos model | the smallest attack surface | Panoptes maintains every package |

Reopen when a design partner requires NVIDIA AI Enterprise support or a
FIPS-validated OS environment. Then ship the same units on an Ubuntu LTS
or RHEL image-mode host.

## Each release ships its own evidence

1. A signed, immutable image with A/B slots and rollback.
2. A CycloneDX SBOM and VEX, generated from the image's closure.
3. An advisory feed for Panoptes' packages, in a format scanners ingest.
4. A STIG mapping, starting from the Anduril NixOS STIG.
5. FIPS through a validated module, with Panoptes' affirmation of the
   operating environment.
6. A support calendar with at least 5 years per LTS release.

sbomnix and bombon already generate item 2 from Nix closures. A bought LTS
stream supplies most of items 3 and 6.

The kernel limits item 6. kernel.org maintains its LTS kernels for about
three years, so 6.18 ends in December 2028; only CIP's 6.12 runs past five
years. [kernel-and-inference.md](kernel-and-inference.md) has the choice.

## Sources

Checked on 2026-10-26.

[pve-psa]: https://forum.proxmox.com/threads/general-faq-about-proxmox-security-advisories.149328/latest
[truenas]: http://www.truenas.com/blog/truenas-fangtooth-25-04-2/
[talos-sbom]: https://docs.siderolabs.com/talos/v1.14/learn-more/enterprise-image-factory
[bottlerocket]: https://aws.amazon.com/bottlerocket/faqs/
[esxi-fips]: https://csrc.nist.gov/projects/cryptographic-module-validation-program/certificate/4815
[stig]: https://ncp.nist.gov/checklist/1260
[stig-fips]: https://www.stigviewer.com/stigs/anduril_nixos/2025-08-19/finding/V-268168
[jetpack-nixos]: https://github.com/anduril/jetpack-nixos
[ghaf]: https://ghaf.tii.ae/ghaf/overview/arch/adr/minimal-host/
[flakehub-fedramp]: https://determinate.systems/blog/flakehub-fedramp-high
[cra]: https://digital-strategy.ec.europa.eu/en/policies/cyber-resilience-act
[cra-13]: https://www.springlex.eu/en/packages/cra/cra-regulation/article-13/
[nixos-2605]: https://nixos.org/blog/announcements/2026/nixos-2605/
[nixos-sec]: https://nixos.org/community/teams/security/
[nixos-fin]: https://discourse.nixos.org/t/nixos-foundation-financial-report-2025/79925
[dsp]: https://determinate.systems/secure-packages/
[dsp-docs]: https://docs.determinate.systems/secure-packages/
[ctrl-os]: https://cyberus-technology.de/en/ctrlos/lts
[wiz]: https://www.wiz.io/environments/operating-systems
[osv]: https://ossf.github.io/osv-schema/
[purl]: https://raw.githubusercontent.com/package-url/purl-spec/main/purl-types-index.json
[sbomnix]: https://github.com/tiiuae/sbomnix
[bombon]: https://github.com/nikstur/bombon
[openssl-fips]: https://openssl-library.org/news/fips-cve/index.html
[nvaie]: https://docs.nvidia.com/ai-enterprise/release-7/latest/support/support-matrix-7/7.8.html
[debian-lts]: https://wiki.debian.org/LTS
[ubuntu-cycle]: https://ubuntu.com/about/release-cycle
[rhel-cycle]: https://access.redhat.com/support/policy/updates/errata
