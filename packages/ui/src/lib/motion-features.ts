// The Motion animation engine, loaded after first paint by <LazyMotion> in App.tsx instead of being
// part of the main bundle. `domMax` includes layout animation, which the bottom navigation uses.
export { domMax as default } from 'motion/react'
