FROM node:20-alpine AS builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM nginx:1.27-alpine
COPY --from=builder /app/dist /usr/share/nginx/html
# As a template so ${GATEWAY_TOKEN} is substituted from the env at container
# start (nginx image runs envsubst over /etc/nginx/templates/*.template).
COPY nginx.conf /etc/nginx/templates/default.conf.template
ENV GATEWAY_TOKEN=""
EXPOSE 9200
CMD ["nginx", "-g", "daemon off;"]
