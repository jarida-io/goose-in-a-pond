//go:build !android

package mobile

// Other platforms resolve names for themselves. Darwin and iOS have a working
// platform resolver, and a desktop or server host has real resolver
// configuration, so nothing is installed and the backend keeps its own defaults.
