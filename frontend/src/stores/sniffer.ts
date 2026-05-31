import { createSignal } from 'solid-js'
import type { SnifferResource } from '../types'

const [resources, setResources] = createSignal<SnifferResource[]>([])
const [active, setActive] = createSignal(true)

export { resources, setResources, active, setActive }

export const $snifferResources = {
  get: resources,
  set: setResources,
}

export const $snifferActive = {
  get: active,
  set: setActive,
}