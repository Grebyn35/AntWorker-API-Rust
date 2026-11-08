# syntax=docker/dockerfile:1.27.1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e

# The reference deployment's Postgres (D44): the official image, with pgBackRest, which archives
# the write-ahead log and takes the base backups to a bucket off the server (OPS-15).
# `just image` builds it for linux/amd64 and linux/arm64, beside the API's image (OPS-12), and
# docs/operations/backups.md describes how the deployment runs it.
FROM postgres:18.6@sha256:3a4264520e121176619f54ff867d12774bc81e95e7b84db58d63bccdb6144b81

# pgBackRest comes from the PostgreSQL Global Development Group's repository, which the official
# image already trusts, at an exact version. ca-certificates holds the certificate authorities
# that a bucket's certificate is checked against, unless the deployment names an authority of its
# own (OPS-17); the official image has none. jq reads pgBackRest's info for the retention that
# keeps the backups restores started from (retention.jq). apt and libapt-pkg7.0 come from Debian's
# security updates at 3.0.4, which fixes CVE-2026-109518, rated high, in the 3.0.3 that the
# official image's build holds, and zlib1g at 1.3.1-1+deb13u1, which fixes CVE-2026-76844 and
# CVE-2026-85091, rated high, in its 1.3.1-1+b1 (OPS-13). The build fails once the official image
# holds either fix, so each leaves the install with the update of the digest that brings it.
RUN if dpkg --compare-versions "$(dpkg-query --show --showformat='${Version}' apt)" ge 3.0.4; then \
        echo "postgres:18.6 holds apt 3.0.4 or later: remove apt and libapt-pkg7.0 from this install" >&2; exit 1; \
    fi \
    && if dpkg --compare-versions "$(dpkg-query --show --showformat='${Version}' zlib1g)" ge 1:1.3.dfsg+really1.3.1-1+deb13u1; then \
        echo "postgres:18.6 holds zlib1g 1.3.1-1+deb13u1 or later: remove it from this install" >&2; exit 1; \
    fi \
    && apt-get update \
    && apt-get install --yes --no-install-recommends pgbackrest=2.59.3-1.pgdg13+1 ca-certificates=20250419+deb13u1 jq=1.7.1-6+deb13u4 apt=3.0.4 libapt-pkg7.0=3.0.4 zlib1g=1:1.3.dfsg+really1.3.1-1+deb13u1 \
    && rm -rf /var/lib/apt/lists/* /var/log/apt /var/log/dpkg.log /var/cache/apt/*.bin

COPY pgbackrest.conf /etc/pgbackrest/pgbackrest.conf
COPY retention.jq /usr/local/lib/pgbackrest/retention.jq
COPY --chmod=0755 bin/ /usr/local/bin/
# The status directory, a volume that the backups share with the collector, belongs to the user
# that Postgres and pgBackRest run as.
RUN install --directory --owner=postgres --group=postgres /var/lib/backup-status
# Postgres's own programs come first on the PATH, before their names in /usr/bin, which run
# Debian's pg_wrapper: a Perl script that picks among the installed versions of Postgres, of which
# the image has one. Where the image runs through QEMU, as on a machine of another platform,
# Perl's start makes each psql, and each health check's pg_isready, take about a second of a
# processor, not a tenth.
ENV PATH=/usr/lib/postgresql/$PG_MAJOR/bin:$PATH
