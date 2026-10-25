<script lang="ts">
let {page}:{page:{offset:number;limit:number;total:number;pages:number;go:(offset:number)=>void}}=$props();
</script>
{#if page.total>0}<div class="pager"><button disabled={page.offset===0} onclick={()=>page.go(0)}>First</button><button disabled={page.offset===0} onclick={()=>page.go(Math.max(0,page.offset-page.limit))}>Previous</button><span>Page {Math.floor(page.offset/page.limit)+1} of {page.pages} · {page.total} total</span><button disabled={page.offset+page.limit>=page.total} onclick={()=>page.go(page.offset+page.limit)}>Next</button><button disabled={page.offset+page.limit>=page.total} onclick={()=>page.go((page.pages-1)*page.limit)}>Last</button></div>{/if}
