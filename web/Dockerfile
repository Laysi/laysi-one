FROM node:26-slim AS build
WORKDIR /app

COPY package.json package-lock.json* ./
RUN npm ci --no-audit --no-fund 2>/dev/null || npm install --no-audit --no-fund

COPY . .
RUN npm run build

FROM caddy:2-alpine
COPY Caddyfile /etc/caddy/Caddyfile
COPY --from=build /app/dist /srv
# Fail the build rather than the deployment when the config is malformed.
RUN caddy validate --config /etc/caddy/Caddyfile
EXPOSE 80
