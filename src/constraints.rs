use crate::{CryptographicSponge, DomainSeparatedSponge, DomainSeparator, FieldElementSize};
use ark_ff::{PrimeField, ToConstraintField};
use ark_nonnative_field::params::{get_params, OptimizationType};
use ark_nonnative_field::{AllocatedNonNativeFieldVar, NonNativeFieldVar};
use ark_r1cs_std::alloc::AllocVar;
use ark_r1cs_std::bits::boolean::Boolean;
use ark_r1cs_std::bits::uint8::UInt8;
use ark_r1cs_std::fields::fp::{AllocatedFp, FpVar};
use ark_r1cs_std::R1CSVar;
use ark_relations::lc;
use ark_relations::r1cs::{ConstraintSystemRef, LinearCombination, SynthesisError};
use ark_std::vec::Vec;
use std::marker::PhantomData;

pub fn bits_le_to_nonnative<'a, F: PrimeField, CF: PrimeField>(
    cs: ConstraintSystemRef<CF>,
    all_nonnative_bits_le: impl IntoIterator<Item = &'a Vec<Boolean<CF>>>,
) -> Result<Vec<NonNativeFieldVar<F, CF>>, SynthesisError> {
    let all_nonnative_bits_le = all_nonnative_bits_le.into_iter().collect::<Vec<_>>();

    let mut max_nonnative_bits = all_nonnative_bits_le
        .iter()
        .fold(0usize, |max_num_bits, bits| max_num_bits.max(bits.len()));

    let mut lookup_table = Vec::<Vec<CF>>::new();
    let mut cur = F::one();
    for _ in 0..max_nonnative_bits {
        let repr = AllocatedNonNativeFieldVar::<F, CF>::get_limbs_representations(
            &cur,
            OptimizationType::Constraints,
        )?;
        lookup_table.push(repr);
        cur.double_in_place();
    }

    let params = get_params(
        F::size_in_bits(),
        CF::size_in_bits(),
        OptimizationType::Constraints,
    );

    let mut output = Vec::with_capacity(all_nonnative_bits_le.len());
    for nonnative_bits_le in all_nonnative_bits_le {
        let mut val = vec![CF::zero(); params.num_limbs];
        let mut lc = vec![LinearCombination::<CF>::zero(); params.num_limbs];

        for (j, bit) in nonnative_bits_le.iter().enumerate() {
            if bit.value().unwrap_or_default() {
                for (k, val) in val.iter_mut().enumerate().take(params.num_limbs) {
                    *val += &lookup_table[j][k];
                }
            }

            #[allow(clippy::needless_range_loop)]
            for k in 0..params.num_limbs {
                lc[k] = &lc[k] + bit.lc() * lookup_table[j][k];
            }
        }

        let mut limbs = Vec::new();
        for k in 0..params.num_limbs {
            let gadget =
                AllocatedFp::new_witness(ark_relations::ns!(cs, "alloc"), || Ok(val[k])).unwrap();
            lc[k] = lc[k].clone() - (CF::one(), gadget.variable);
            cs.enforce_constraint(lc!(), lc!(), lc[k].clone()).unwrap();
            limbs.push(FpVar::<CF>::from(gadget));
        }

        output.push(NonNativeFieldVar::<F, CF>::Var(
            AllocatedNonNativeFieldVar::<F, CF> {
                cs: cs.clone(),
                limbs,
                num_of_additions_over_normal_form: CF::zero(),
                is_in_the_normal_form: true,
                target_phantom: Default::default(),
            },
        ));
    }

    Ok(output)
}

/// The interface for a cryptographic sponge.
/// A sponge can `absorb` or take in inputs and later `squeeze` or output bytes or field elements.
/// The outputs are dependent on previous `absorb` and `squeeze` calls.
pub trait CryptographicSpongeVar<CF: PrimeField, S: CryptographicSponge<CF>>: Clone {
    /// Initialize a new instance of the sponge.
    fn new(cs: ConstraintSystemRef<CF>) -> Self;

    fn cs(&self) -> ConstraintSystemRef<CF>;

    /// Absorb an input into the sponge.
    fn absorb(&mut self, input: &[FpVar<CF>]) -> Result<(), SynthesisError>;

    /// Squeeze `num_bytes` bytes from the sponge.
    fn squeeze_bytes(&mut self, num_bytes: usize) -> Result<Vec<UInt8<CF>>, SynthesisError>;

    /// Squeeze `num_bit` bits from the sponge.
    fn squeeze_bits(&mut self, num_bits: usize) -> Result<Vec<Boolean<CF>>, SynthesisError>;

    /// Squeeze `num_elements` field elements from the sponge.
    fn squeeze_field_elements(
        &mut self,
        num_elements: usize,
    ) -> Result<Vec<FpVar<CF>>, SynthesisError>;

    fn squeeze_nonnative_field_elements_with_sizes<F: PrimeField>(
        &mut self,
        sizes: &[FieldElementSize],
    ) -> Result<(Vec<NonNativeFieldVar<F, CF>>, Vec<Vec<Boolean<CF>>>), SynthesisError> {
        if sizes.len() == 0 {
            return Ok((Vec::new(), Vec::new()));
        }

        let cs = self.cs();

        let mut total_bits = sizes.iter().fold(0usize, |total_bits, size| {
            return total_bits + size.num_bits::<F>();
        });

        let bits = self.squeeze_bits(total_bits)?;

        let mut dest_bits = Vec::<Vec<Boolean<CF>>>::with_capacity(sizes.len());

        let mut bits_window = bits.as_slice();
        for size in sizes {
            let num_bits = size.num_bits::<F>();
            let nonnative_bits_le = bits_window[..num_bits].to_vec();
            bits_window = &bits_window[num_bits..];

            dest_bits.push(nonnative_bits_le);
        }

        let dest_gadgets = bits_le_to_nonnative(cs, dest_bits.iter())?;

        Ok((dest_gadgets, dest_bits))
    }

    fn squeeze_nonnative_field_elements<F: PrimeField>(
        &mut self,
        num_elements: usize,
    ) -> Result<(Vec<NonNativeFieldVar<F, CF>>, Vec<Vec<Boolean<CF>>>), SynthesisError> {
        self.squeeze_nonnative_field_elements_with_sizes::<F>(
            vec![FieldElementSize::Full; num_elements].as_slice(),
        )
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = "D: DomainSeparator"))]
pub struct DomainSeparatedSpongeVar<
    CF: PrimeField,
    S: CryptographicSponge<CF>,
    SV: CryptographicSpongeVar<CF, S>,
    D: DomainSeparator,
> {
    sponge: SV,
    domain_separated: bool,

    _affine_phantom: PhantomData<CF>,
    _sponge_phantom: PhantomData<S>,
    _domain_separator_phantom: PhantomData<D>,
}

impl<CF, S, SV, D> DomainSeparatedSpongeVar<CF, S, SV, D>
where
    CF: PrimeField,
    S: CryptographicSponge<CF>,
    SV: CryptographicSpongeVar<CF, S>,
    D: DomainSeparator,
{
    fn try_separate_domain(&mut self) -> Result<(), SynthesisError> {
        if !self.domain_separated {
            let elems: Vec<CF> = D::domain().to_field_elements().unwrap();
            let elem_vars = elems
                .into_iter()
                .map(|elem| FpVar::Constant(elem))
                .collect::<Vec<_>>();

            self.sponge.absorb(elem_vars.as_slice())?;

            self.domain_separated = true;
        }
        Ok(())
    }
}

impl<CF, S, SV, D> CryptographicSpongeVar<CF, DomainSeparatedSponge<CF, S, D>>
    for DomainSeparatedSpongeVar<CF, S, SV, D>
where
    CF: PrimeField,
    S: CryptographicSponge<CF>,
    SV: CryptographicSpongeVar<CF, S>,
    D: DomainSeparator,
{
    fn new(cs: ConstraintSystemRef<CF>) -> Self {
        Self {
            sponge: SV::new(cs),
            domain_separated: false,
            _affine_phantom: PhantomData,
            _sponge_phantom: PhantomData,
            _domain_separator_phantom: PhantomData,
        }
    }

    fn cs(&self) -> ConstraintSystemRef<CF> {
        self.sponge.cs()
    }

    fn absorb(&mut self, input: &[FpVar<CF>]) -> Result<(), SynthesisError> {
        self.try_separate_domain()?;
        self.sponge.absorb(input)
    }

    fn squeeze_bytes(&mut self, num_bytes: usize) -> Result<Vec<UInt8<CF>>, SynthesisError> {
        self.try_separate_domain()?;
        self.sponge.squeeze_bytes(num_bytes)
    }

    fn squeeze_bits(&mut self, num_bits: usize) -> Result<Vec<Boolean<CF>>, SynthesisError> {
        self.try_separate_domain()?;
        self.sponge.squeeze_bits(num_bits)
    }

    fn squeeze_field_elements(
        &mut self,
        num_elements: usize,
    ) -> Result<Vec<FpVar<CF>>, SynthesisError> {
        self.try_separate_domain()?;
        self.sponge.squeeze_field_elements(num_elements)
    }

    fn squeeze_nonnative_field_elements_with_sizes<F: PrimeField>(
        &mut self,
        sizes: &[FieldElementSize],
    ) -> Result<(Vec<NonNativeFieldVar<F, CF>>, Vec<Vec<Boolean<CF>>>), SynthesisError> {
        self.try_separate_domain()?;
        self.sponge
            .squeeze_nonnative_field_elements_with_sizes(sizes)
    }
}

#[cfg(test)]
pub mod tests {
    use crate::collect_sponge_bytes;
    use crate::collect_sponge_field_elements;
    use crate::constraints::CryptographicSpongeVar;
    use crate::poseidon::constraints::PoseidonSpongeVar;
    use crate::poseidon::PoseidonSponge;
    use crate::Absorbable;
    use crate::{CryptographicSponge, FieldElementSize};
    use ark_ed_on_bls12_381::{Fq, Fr};
    use ark_ff::{One, ToConstraintField};
    use ark_ff::{PrimeField, Zero};
    use ark_r1cs_std::fields::fp::FpVar;
    use ark_r1cs_std::fields::FieldVar;
    use ark_r1cs_std::R1CSVar;
    use ark_relations::r1cs::ConstraintSystem;

    type F = Fr;
    type CF = Fq;

    pub struct Test<F: PrimeField> {
        a: Vec<u8>,
        b: u128,
        c: F,
    }

    impl<F: PrimeField> Absorbable<F> for Test<F>
    where
        F: Absorbable<F>,
    {
        fn to_sponge_bytes(&self) -> Vec<u8> {
            collect_sponge_bytes!(F, self.a, self.b, self.c)
        }

        fn to_sponge_field_elements(&self) -> Vec<F> {
            collect_sponge_field_elements!(F, self.a, self.b, self.c)
        }
    }

    #[test]
    fn test_ae() {
        let a = Test {
            a: vec![],
            b: 0,
            c: Fr::zero(),
        };
        let mut s = PoseidonSponge::<Fr>::new();
        s.absorb(&a);
    }

    /*
    #[test]
    fn test_a() {
        let a = vec![0u8, 5, 6, 2, 3, 7, 2];
        let mut s = PoseidonSponge::<CF>::new();
        s.absorb(&a);
    }

    #[test]
    fn test_squeeze_nonnative_field_elements() {
        let cs = ConstraintSystem::<CF>::new_ref();
        let mut s = PoseidonSponge::<CF>::new();
        s.absorb(&CF::one());

        let mut s_var = PoseidonSpongeVar::<CF>::new(cs.clone());
        s_var.absorb(&[FpVar::<CF>::one()]);

        let out: Vec<F> = s.squeeze_nonnative_field_elements_with_sizes::<F>(&[
            FieldElementSize::Truncated { num_bits: 128 },
            FieldElementSize::Truncated { num_bits: 180 },
            FieldElementSize::Full,
            FieldElementSize::Truncated { num_bits: 128 },
        ]);
        let out_var = s_var
            .squeeze_nonnative_field_elements_with_sizes::<F>(&[
                FieldElementSize::Truncated { num_bits: 128 },
                FieldElementSize::Truncated { num_bits: 180 },
                FieldElementSize::Full,
                FieldElementSize::Truncated { num_bits: 128 },
            ])
            .unwrap();

        println!("{:?}", out);
        println!("{:?}", out_var.0.value().unwrap());

        /*
        let out = s
            .squeeze_nonnative_field_elements::<F>(&[
                FieldElementSize::Truncated { num_bits: 128 },
                FieldElementSize::Truncated { num_bits: 128 },
            ])
            .unwrap();
        println!("{:?}", out.0.value().unwrap());

         */
    }*/
}