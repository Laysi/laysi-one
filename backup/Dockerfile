# The backup job: pg_dump to speak to PostgreSQL, age to encrypt, aws to
# write anywhere that speaks S3. It is a separate image because none of these
# belong in the server image, which holds a single static binary.
FROM alpine:3.24

RUN apk add --no-cache postgresql18-client age aws-cli coreutils

COPY run.sh /usr/local/bin/run.sh
RUN chmod +x /usr/local/bin/run.sh

ENTRYPOINT ["/usr/local/bin/run.sh"]
