<script lang="ts">
	import '@fontsource-variable/geist'
	import '@fontsource-variable/geist-mono'
	import '../app.css'
	import '$lib/prose.css'
	import '$lib/signature.css'

	import { page } from '$app/state'
	import Header from '$lib/components/Header.svelte'
	import MobileMenu from '$lib/components/MobileMenu.svelte'
	import Search from '$lib/components/Search.svelte'
	import { unbase } from '$lib/href'
	import { sidebarPath } from '$lib/nav'

	let { data, children } = $props()

	let searching = $state(false)
	let menu = $state(false)

	const book = $derived((page.data.page as { book?: string } | undefined)?.book ?? '')
	const path = $derived(unbase(page.url.pathname))

	const tabs = $derived(
		data.books.map((part: { title: string; href: string }) => ({
			...part,
			active: part.title === book,
		})),
	)

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
	books={data.books}
	book={book || 'Handbook'}
	current={sidebarPath(path)}
	onsearch={() => (searching = true)}
/>
