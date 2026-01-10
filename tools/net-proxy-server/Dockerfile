FROM node:20-alpine

WORKDIR /app
COPY package.json package.json
RUN npm install --omit=dev

COPY src ./src

ENV AERO_PROXY_HOST=0.0.0.0
ENV AERO_PROXY_PORT=8080

EXPOSE 8080
CMD ["node", "src/cli.js"]
