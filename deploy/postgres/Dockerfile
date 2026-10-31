# syntax=docker/dockerfile:1.27.1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e

# The reference deployment's Postgres (D44): the official image, with pgBackRest, which archives
# the write-ahead log and takes the base backups to a bucket off the server (OPS-17).
# `just image` builds it for linux/amd64 and linux/arm64, beside the API's image (OPS-14), and
# docs/operations/backups.md describes how the deployment runs it.
FROM postgres:18.6@sha256:979bccfde35445db618c65107d27d08c6c21339d8dc3ad4c830c6633947be0e4

# pgBackRest comes from the PostgreSQL Global Development Group's repository, which the official
# image already trusts, at an exact version. ca-certificates holds the certificate authorities
# that a bucket's certificate is checked against, unless the deployment names an authority of its
# own (OPS-20); the official image has none.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends pgbackrest=2.59.3-1.pgdg13+1 ca-certificates=20250419+deb13u1 \
    && rm -rf /var/lib/apt/lists/* /var/log/apt /var/log/dpkg.log /var/cache/apt/*.bin

COPY pgbackrest.conf /etc/pgbackrest/pgbackrest.conf
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
