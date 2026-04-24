.PHONY: up down logs restart reset status build

up:
	docker compose up -d

down:
	docker compose down

logs:
	docker compose logs -f

restart:
	docker compose restart

reset:
	docker compose down -v

status:
	docker compose ps

build:
	docker compose build --no-cache
