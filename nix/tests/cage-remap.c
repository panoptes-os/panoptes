/*
 * remap: map an xdg toplevel, unmap it by committing a NULL buffer, then
 * map it again. xdg-shell allows this: after the NULL-buffer commit the
 * client starts over with an initial commit.
 */

#define _GNU_SOURCE

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>

#include "xdg-shell-client-protocol.h"

static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm_base;

static bool configured;
static uint32_t configure_serial;
static int32_t width = 640, height = 480;

static void
wm_base_ping(void *data, struct xdg_wm_base *base, uint32_t serial)
{
	xdg_wm_base_pong(base, serial);
}

static const struct xdg_wm_base_listener wm_base_listener = {
	.ping = wm_base_ping,
};

static void
surface_configure(void *data, struct xdg_surface *xdg_surface, uint32_t serial)
{
	configured = true;
	configure_serial = serial;
}

static const struct xdg_surface_listener surface_listener = {
	.configure = surface_configure,
};

static void
toplevel_configure(void *data, struct xdg_toplevel *toplevel, int32_t w, int32_t h, struct wl_array *states)
{
	if (w > 0 && h > 0) {
		width = w;
		height = h;
	}
}

static void
toplevel_close(void *data, struct xdg_toplevel *toplevel)
{
}

static const struct xdg_toplevel_listener toplevel_listener = {
	.configure = toplevel_configure,
	.close = toplevel_close,
};

static void
handle_global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version)
{
	if (strcmp(interface, wl_compositor_interface.name) == 0) {
		compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
	} else if (strcmp(interface, wl_shm_interface.name) == 0) {
		shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
	} else if (strcmp(interface, xdg_wm_base_interface.name) == 0) {
		wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
		xdg_wm_base_add_listener(wm_base, &wm_base_listener, NULL);
	}
}

static void
handle_global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
}

static const struct wl_registry_listener registry_listener = {
	.global = handle_global,
	.global_remove = handle_global_remove,
};

static struct wl_buffer *
create_buffer(void)
{
	int stride = width * 4;
	size_t size = (size_t) stride * height;
	int fd = memfd_create("remap", MFD_CLOEXEC);
	if (fd < 0 || ftruncate(fd, size) < 0) {
		perror("remap: memfd");
		exit(1);
	}
	uint32_t *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (pixels == MAP_FAILED) {
		perror("remap: mmap");
		exit(1);
	}
	for (size_t i = 0; i < size / 4; i++) {
		pixels[i] = 0xff3366cc;
	}
	munmap(pixels, size);

	struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, size);
	struct wl_buffer *buffer = wl_shm_pool_create_buffer(pool, 0, width, height, stride, WL_SHM_FORMAT_XRGB8888);
	wl_shm_pool_destroy(pool);
	close(fd);
	return buffer;
}

/* The initial commit, its configure, then a buffer. */
static struct wl_buffer *
map(struct wl_display *display, struct wl_surface *surface, struct xdg_surface *xdg_surface)
{
	configured = false;
	wl_surface_commit(surface);
	while (!configured) {
		if (wl_display_dispatch(display) < 0) {
			fprintf(stderr, "remap: lost the compositor\n");
			exit(1);
		}
	}
	xdg_surface_ack_configure(xdg_surface, configure_serial);
	struct wl_buffer *buffer = create_buffer();
	wl_surface_attach(surface, buffer, 0, 0);
	wl_surface_commit(surface);
	if (wl_display_roundtrip(display) < 0) {
		fprintf(stderr, "remap: lost the compositor\n");
		exit(1);
	}
	return buffer;
}

int
main(void)
{
	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		fprintf(stderr, "remap: cannot connect to the Wayland display\n");
		return 1;
	}
	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, NULL);
	wl_display_roundtrip(display);
	if (!compositor || !shm || !wm_base) {
		fprintf(stderr, "remap: missing wl_compositor, wl_shm or xdg_wm_base\n");
		return 1;
	}

	struct wl_surface *surface = wl_compositor_create_surface(compositor);
	struct xdg_surface *xdg_surface = xdg_wm_base_get_xdg_surface(wm_base, surface);
	xdg_surface_add_listener(xdg_surface, &surface_listener, NULL);
	struct xdg_toplevel *toplevel = xdg_surface_get_toplevel(xdg_surface);
	xdg_toplevel_add_listener(toplevel, &toplevel_listener, NULL);
	xdg_toplevel_set_title(toplevel, "remap");

	struct wl_buffer *first = map(display, surface, xdg_surface);
	printf("mapped %dx%d\n", width, height);
	fflush(stdout);

	wl_surface_attach(surface, NULL, 0, 0);
	wl_surface_commit(surface);
	wl_display_roundtrip(display);
	printf("unmapped\n");
	fflush(stdout);

	struct wl_buffer *second = map(display, surface, xdg_surface);
	printf("mapped again\n");
	fflush(stdout);

	wl_buffer_destroy(first);
	wl_buffer_destroy(second);
	xdg_toplevel_destroy(toplevel);
	xdg_surface_destroy(xdg_surface);
	wl_surface_destroy(surface);
	xdg_wm_base_destroy(wm_base);
	wl_shm_destroy(shm);
	wl_compositor_destroy(compositor);
	wl_registry_destroy(registry);
	wl_display_disconnect(display);
	return 0;
}
