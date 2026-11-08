//! Runs a model's program over one caller-owned arena. Activations are channels-last
//! `[H + 2][W + 2][C]` with a zero halo, so conv inner loops never bounds-check.

use super::fbank::NUM_BINS;
use super::kernels::{self, ConvArgs, Rung};
use super::model::{Model, Op, OpKind, MAX_SLOTS, NO_I8};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub h: usize,
    pub w: usize,
    pub c: usize,
}

impl Shape {
    /// Floats including the halo.
    pub fn padded(&self) -> usize {
        (self.h + 2) * (self.w + 2) * self.c
    }
}

/// Where everything lives in the arena for one utterance length.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub frames: usize,
    slot_off: [usize; MAX_SLOTS],
    pool_off: usize,
    pub pool_len: usize,
    /// int8 scratch for the quantised conv input, in f32 units of the arena.
    q8_off: usize,
    q8_len: usize,
    /// Arena size in f32s.
    pub arena: usize,
    /// Multiply-accumulates for one forward pass at this length.
    pub macs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Pooling needs >= 2 time steps after the stride-8 trunk: 9+ frames (~0.1 s).
    TooShort,
    ArenaTooSmall,
    OutTooSmall,
}

/// Output spatial size of a convolution: floor((in + 2p - k) / s) + 1.
fn conv_out(n: usize, k: usize, s: usize, p: usize) -> usize {
    (n + 2 * p - k) / s + 1
}

pub fn plan(model: &Model, frames: usize) -> Result<Plan, Error> {
    let mut shape = [Shape::default(); MAX_SLOTS];
    let mut cap = [0usize; MAX_SLOTS];
    shape[0] = Shape {
        h: NUM_BINS,
        w: frames,
        c: 1,
    };
    cap[0] = shape[0].padded();
    let (mut pool_len, mut macs, mut q8_bytes) = (0usize, 0u64, 0usize);

    for i in 0..model.n_ops() {
        let op = model.op(i);
        match op.kind {
            OpKind::Conv2d => {
                let s = shape[op.src as usize];
                let (k, st, p) = (op.k as usize, op.stride as usize, op.pad());
                if s.h + 2 * p < k || s.w + 2 * p < k {
                    return Err(Error::TooShort);
                }
                let o = Shape {
                    h: conv_out(s.h, k, st, p),
                    w: conv_out(s.w, k, st, p),
                    c: op.cout as usize,
                };
                shape[op.dst as usize] = o;
                cap[op.dst as usize] = cap[op.dst as usize].max(o.padded());
                macs += (o.h * o.w * o.c * k * k * op.cin as usize) as u64;
                if op.w8 != NO_I8 {
                    q8_bytes = q8_bytes.max(s.padded());
                }
            }
            OpKind::Depthwise => {
                let s = shape[op.src as usize];
                let (k, st, p) = (op.k as usize, op.stride as usize, op.pad());
                if s.h + 2 * p < k || s.w + 2 * p < k {
                    return Err(Error::TooShort);
                }
                let o = Shape {
                    h: conv_out(s.h, k, st, p),
                    w: conv_out(s.w, k, st, p),
                    c: s.c,
                };
                shape[op.dst as usize] = o;
                cap[op.dst as usize] = cap[op.dst as usize].max(o.padded());
                macs += (o.h * o.w * o.c * k * k) as u64;
            }
            OpKind::TstpPool => {
                let s = shape[op.src as usize];
                if s.w < 2 {
                    return Err(Error::TooShort);
                }
                pool_len = 2 * s.h * s.c;
            }
            OpKind::GapPool => pool_len = shape[op.src as usize].c,
            OpKind::Linear => macs += op.cin as u64 * op.cout as u64,
        }
    }

    // Slots back to back, each rounded to 16 floats (64 bytes) so every slot
    // starts on a cache line when the arena does.
    let mut slot_off = [0usize; MAX_SLOTS];
    let mut at = 0usize;
    for (off, c) in slot_off.iter_mut().zip(cap.iter()).take(model.n_slots) {
        *off = at;
        at += c.div_ceil(16) * 16;
    }
    let pool_off = at;
    at += pool_len.div_ceil(16) * 16;
    let q8_off = at;
    let q8_len = q8_bytes.div_ceil(4);
    at += q8_len.div_ceil(16) * 16;
    Ok(Plan {
        frames,
        slot_off,
        pool_off,
        pool_len,
        q8_off,
        q8_len,
        arena: at,
        macs,
    })
}

/// Called after every op with its output (halo layout for convs) and shape: per-layer parity and
/// timing hang off this.
pub type Observer<'o> = &'o mut dyn FnMut(usize, &Op, &[f32], Shape);

/// Forward pass. `feats` is `[frames][80]` row-major with CMN applied.
pub fn run(
    model: &Model,
    plan: &Plan,
    rung: Rung,
    feats: &[f32],
    arena: &mut [f32],
    out: &mut [f32],
    mut observe: Option<Observer>,
) -> Result<(), Error> {
    if arena.len() < plan.arena {
        return Err(Error::ArenaTooSmall);
    }
    if out.len() < model.emb_dim {
        return Err(Error::OutTooSmall);
    }
    let t = plan.frames;
    let mut shape = [Shape::default(); MAX_SLOTS];
    shape[0] = Shape {
        h: NUM_BINS,
        w: t,
        c: 1,
    };

    // Input: transpose [T][80] into [80 + 2][T + 2][1] with a zero halo.
    {
        let x = &mut arena[plan.slot_off[0]..plan.slot_off[0] + shape[0].padded()];
        x.fill(0.0);
        for (ti, row) in feats.chunks_exact(NUM_BINS).take(t).enumerate() {
            for (f, &v) in row.iter().enumerate() {
                x[(f + 1) * (t + 2) + ti + 1] = v;
            }
        }
    }

    let base = arena.as_mut_ptr();
    for i in 0..model.n_ops() {
        let op = model.op(i);
        match op.kind {
            OpKind::Conv2d => {
                let s = shape[op.src as usize];
                let (k, st, p) = (op.k as usize, op.stride as usize, op.pad());
                let o = Shape {
                    h: conv_out(s.h, k, st, p),
                    w: conv_out(s.w, k, st, p),
                    c: op.cout as usize,
                };
                // SAFETY: slots are disjoint ranges of `arena` (plan lays them end to end) and
                // src != dst (validated), so these never alias.
                let (src, dst) = unsafe {
                    (
                        core::slice::from_raw_parts(
                            base.add(plan.slot_off[op.src as usize]),
                            s.padded(),
                        ),
                        core::slice::from_raw_parts_mut(
                            base.add(plan.slot_off[op.dst as usize]),
                            o.padded(),
                        ),
                    )
                };
                if !op.accum() {
                    zero_halo(dst, o);
                }
                let kw = op.cout as usize * k * k * op.cin as usize;
                let args = ConvArgs {
                    src,
                    hi: s.h,
                    wi: s.w,
                    cin: op.cin as usize,
                    ho: o.h,
                    wo: o.w,
                    cout: o.c,
                    k,
                    stride: st,
                    pad: p,
                    w: &model.w32[op.w as usize..op.w as usize + kw],
                    b: &model.w32[op.b as usize..op.b as usize + o.c],
                    relu: op.relu(),
                    accum: op.accum(),
                };
                #[cfg(target_arch = "aarch64")]
                if rung == Rung::I8Sdot && op.w8 != NO_I8 {
                    // SAFETY: the q8 region follows the pool region, disjoint from every slot;
                    // plan sized it for the largest int8 conv input.
                    let q8 = unsafe {
                        core::slice::from_raw_parts_mut(
                            base.add(plan.q8_off) as *mut i8,
                            plan.q8_len * 4,
                        )
                    };
                    let q8 = &mut q8[..s.padded()];
                    let (src_scale, offset) = kernels::neon_i8::quantize(src, q8);
                    let s8 = op.s8 as usize;
                    let q = kernels::neon_i8::I8Args {
                        src: q8,
                        src_scale,
                        w: &model.w8[op.w8 as usize..op.w8 as usize + kw],
                        w_scale: &model.w32[s8..s8 + o.c],
                        w_sum: &model.w32[s8 + o.c..s8 + 2 * o.c],
                        offset,
                    };
                    kernels::neon_i8::conv2d(&args, &q, dst);
                } else {
                    kernels::conv2d(rung, &args, dst);
                }
                #[cfg(not(target_arch = "aarch64"))]
                kernels::conv2d(rung, &args, dst);
                shape[op.dst as usize] = o;
                if let Some(f) = observe.as_mut() {
                    f(i, &op, dst, o);
                }
            }
            OpKind::Depthwise => {
                let s = shape[op.src as usize];
                let (k, st, p) = (op.k as usize, op.stride as usize, op.pad());
                let o = Shape {
                    h: conv_out(s.h, k, st, p),
                    w: conv_out(s.w, k, st, p),
                    c: s.c,
                };
                // SAFETY: disjoint slots, src != dst (validated).
                let (src, dst) = unsafe {
                    (
                        core::slice::from_raw_parts(
                            base.add(plan.slot_off[op.src as usize]),
                            s.padded(),
                        ),
                        core::slice::from_raw_parts_mut(
                            base.add(plan.slot_off[op.dst as usize]),
                            o.padded(),
                        ),
                    )
                };
                zero_halo(dst, o);
                let args = kernels::DwArgs {
                    src,
                    wi: s.w,
                    c: s.c,
                    ho: o.h,
                    wo: o.w,
                    k,
                    stride: st,
                    pad: p,
                    w: &model.w32[op.w as usize..op.w as usize + k * k * s.c],
                    b: &model.w32[op.b as usize..op.b as usize + s.c],
                    relu: op.relu(),
                };
                kernels::depthwise(rung, &args, dst);
                shape[op.dst as usize] = o;
                if let Some(f) = observe.as_mut() {
                    f(i, &op, dst, o);
                }
            }
            OpKind::GapPool => {
                let s = shape[op.src as usize];
                // SAFETY: as for TstpPool.
                let (src, pool) = unsafe {
                    (
                        core::slice::from_raw_parts(
                            base.add(plan.slot_off[op.src as usize]),
                            s.padded(),
                        ),
                        core::slice::from_raw_parts_mut(base.add(plan.pool_off), s.c),
                    )
                };
                kernels::gap_pool(src, s.h, s.w, s.c, pool);
                if let Some(f) = observe.as_mut() {
                    f(i, &op, pool, Shape { h: 1, w: 1, c: s.c });
                }
            }
            OpKind::TstpPool => {
                let s = shape[op.src as usize];
                // SAFETY: as above; the pool region follows every slot.
                let (src, pool) = unsafe {
                    (
                        core::slice::from_raw_parts(
                            base.add(plan.slot_off[op.src as usize]),
                            s.padded(),
                        ),
                        core::slice::from_raw_parts_mut(base.add(plan.pool_off), plan.pool_len),
                    )
                };
                kernels::tstp_pool(src, s.h, s.w, s.c, f32::from_bits(op.aux), pool);
                if let Some(f) = observe.as_mut() {
                    f(
                        i,
                        &op,
                        pool,
                        Shape {
                            h: 1,
                            w: 1,
                            c: plan.pool_len,
                        },
                    );
                }
            }
            OpKind::Linear => {
                let (n_in, n_out) = (op.cin as usize, op.cout as usize);
                let pool = &arena[plan.pool_off..plan.pool_off + n_in];
                kernels::linear(
                    &model.w32[op.w as usize..op.w as usize + n_in * n_out],
                    &model.w32[op.b as usize..op.b as usize + n_out],
                    pool,
                    &mut out[..n_out],
                );
                if let Some(f) = observe.as_mut() {
                    f(
                        i,
                        &op,
                        &out[..n_out],
                        Shape {
                            h: 1,
                            w: 1,
                            c: n_out,
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

/// Zero the one-pixel border of a halo-layout activation; the interior is about to be overwritten.
fn zero_halo(x: &mut [f32], s: Shape) {
    let row = (s.w + 2) * s.c;
    x[..row].fill(0.0);
    x[(s.h + 1) * row..(s.h + 2) * row].fill(0.0);
    for y in 1..=s.h {
        x[y * row..y * row + s.c].fill(0.0);
        x[(y + 1) * row - s.c..(y + 1) * row].fill(0.0);
    }
}
