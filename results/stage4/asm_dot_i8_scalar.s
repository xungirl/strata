	.globl	__ZN13strata_vector8distance13dot_i8_scalar17h6ec98534b4641e80E
	.p2align	2
__ZN13strata_vector8distance13dot_i8_scalar17h6ec98534b4641e80E:
Lfunc_begin40:
	.loc	73 59 0
	.cfi_startproc
	sub	sp, sp, #32
	.cfi_def_cfa_offset 32
	stp	x29, x30, [sp, #16]
	add	x29, sp, #16
	.cfi_def_cfa w29, 16
	.cfi_offset w30, -8
	.cfi_offset w29, -16
	.cfi_remember_state
Ltmp5078:
	.loc	73 60 16 prologue_end
	stp	x1, x3, [sp]
Ltmp5079:
	.loc	73 60 5 is_stmt 0
	cmp	x1, x3
	b.ne	LBB40_17
Ltmp5080:
	.loc	72 772 12 is_stmt 1
	cbz	x1, LBB40_4
	cmp	x1, #4
	b.hs	LBB40_5
	.loc	72 0 12 is_stmt 0
	mov	w8, #0
	mov	x9, #0
	.loc	72 772 12
	b	LBB40_14
LBB40_4:
	.loc	72 0 12
	mov	w8, #0
	.loc	72 772 12
	b	LBB40_16
LBB40_5:
	cmp	x1, #32
	b.hs	LBB40_7
	.loc	72 0 12
	mov	x9, #0
	mov	w8, #0
	.loc	72 772 12
	b	LBB40_11
LBB40_7:
	and	x9, x1, #0x7fffffffffffffe0
	add	x8, x2, #16
	add	x10, x0, #16
	movi.2d	v0, #0000000000000000
	mov	x11, x9
	movi.2d	v1, #0000000000000000
Ltmp5081:
LBB40_8:
	.loc	99 286 25 is_stmt 1
	ldp	q2, q3, [x10, #-16]
	ldp	q4, q5, [x8, #-16]
	sdot
	sdot
Ltmp5082:
	.loc	12 763 17
	add	x8, x8, #32
	add	x10, x10, #32
	subs	x11, x11, #32
	b.ne	LBB40_8
Ltmp5083:
	.loc	72 772 12
	add.4s	v0, v1, v0
	addv.4s	s0, v0
	fmov	w8, s0
	cmp	x1, x9
	b.eq	LBB40_16
	tst	x1, #0x1c
	b.eq	LBB40_14
LBB40_11:
	.loc	72 0 12 is_stmt 0
	mov	x11, x9
	.loc	72 772 12
	and	x9, x1, #0x7ffffffffffffffc
	movi.2d	v0, #0000000000000000
	mov.s	v0[0], w8
	sub	x8, x11, x9
	add	x10, x2, x11
	add	x11, x0, x11
Ltmp5084:
LBB40_12:
	.loc	39 79 17 is_stmt 1
	ldr	s1, [x11], #4
	sshll.8h	v1, v1, #0
Ltmp5085:
	.loc	39 79 17 is_stmt 0
	ldr	s2, [x10], #4
	sshll.8h	v2, v2, #0
Ltmp5086:
	.loc	81 53 28 is_stmt 1
	smlal.4s	v0, v2, v1
Ltmp5087:
	.loc	12 763 17
