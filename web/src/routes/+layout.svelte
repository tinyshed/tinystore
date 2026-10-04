<script lang="ts">
	import '@fontsource-variable/geist'
	import '@fontsource-variable/geist-mono'
	import '../app.css'
	import '$lib/prose.css'

	import { page } from '$app/state'
	import Header from '$lib/components/Header.svelte'
	import MobileMenu from '$lib/components/MobileMenu.svelte'
	import Search from '$lib/components/Search.svelte'
	import { unbase } from '$lib/href'
	import { sidebarPath } from '$lib/nav'

	let { data, children } = $props()

	let searching = $state(false)
	let menu = $state(false)

	const section = $derived((page.data.page as { section?: string } | undefined)?.section ?? '')
	const path = $derived(unbase(page.url.pathname))
	const inDocs = $derived(path === '/docs' || path.startsWith('/docs/'))

	const tabs = $derived([
		{ title: 'Docs', href: '/docs', active: inDocs && section.toLowerCase() !== 'reference' },
		{ title: 'Reference', href: data.reference, active: section.toLowerCase() === 'reference' },
	])

	function onkeydown(event: KeyboardEvent) {
		const typing =
			event.target instanceof HTMLElement &&
			(event.target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName))
		if ((event.key === 'k' && (event.metaKey || event.ctrlKey)) || (event.key === '/' && !typing)) {
			event.preventDefault()
			searching = true
		}
	}
</script>

<svelte:window {onkeydown} />

<Header {tabs} onsearch={() => (searching = true)} onmenu={() => (menu = true)} />

{@render children()}

<Search bind:open={searching} />
<MobileMenu
	bind:open={menu}
	nav={data.nav}
	current={sidebarPath(path)}
	onsearch={() => (searching = true)}
/>
