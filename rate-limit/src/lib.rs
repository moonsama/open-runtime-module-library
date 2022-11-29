//! # Rate Limit
//! A module to provide rate limit for arbitrary type Key and integer type Value
//!
//! - [`Config`](./trait.Config.html)
//! - [`Call`](./enum.Call.html)
//! - [`Module`](./struct.Module.html)
//!
//! ## Overview
//!
//! This module is a utility to provide rate limiter for arbitrary type Key and
//! integer type Value, which can config limit rule to produce quota and consume
//! quota, and expose quota consuming checking and whitelist that can bypass
//! checks.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::unused_unit)]

use frame_support::{pallet_prelude::*, traits::UnixTime, transactional, BoundedVec};
use frame_system::pallet_prelude::*;
use orml_traits::{RateLimiter, RateLimiterError};
use scale_info::TypeInfo;
use sp_runtime::traits::{MaybeSerializeDeserialize, SaturatedConversion, Zero};
use sp_std::{prelude::*, vec::Vec};

pub use module::*;
// pub use weights::WeightInfo;

mod mock;
mod tests;
// pub mod weights;

#[frame_support::pallet]
pub mod module {
	use super::*;

	/// Limit rules type.
	#[derive(PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, TypeInfo)]
	pub enum RateLimitRule {
		/// Each `blocks_count` blocks to reset remainer quota to `quota`
		/// amount. is_allowed check return true when the remainer quota gte the
		/// consume amount.
		PerBlocks { blocks_count: u64, quota: u128 },
		/// Each `secs_count` seconds to reset remainer quota to `quota` amount.
		/// is_allowed check return true when the remainer quota gte the consume
		/// amount.
		PerSeconds { secs_count: u64, quota: u128 },
		/// Each `blocks_count` blocks to increase `quota_increment` amount to
		/// remainer quota and keep remainer quota lte `max_quota`. is_allowed
		/// check return true when the remainer quota gte the consume amount.
		TokenBucket {
			blocks_count: u64,
			quota_increment: u128,
			max_quota: u128,
		},
		/// is_allowed check return true always.
		Unlimited,
		/// is_allowed check return false always.
		NotAllowed,
	}

	/// Match rules to fitler key is in bypass whitelist.
	#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Encode, Decode, RuntimeDebug, TypeInfo)]
	pub enum KeyFilter {
		/// If the encoded key is equal to the vec, the key is in whitelist.
		Match(Vec<u8>),
		/// If the encoded key starts with the vec, the key is in whitelist.
		StartsWith(Vec<u8>),
		/// If the encoded key ends with the vec, the key is in whitelist.
		EndsWith(Vec<u8>),
	}

	#[pallet::config]
	pub trait Config: frame_system::Config {
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		/// Origin represented Governance.
		type GovernanceOrigin: EnsureOrigin<Self::RuntimeOrigin>;

		type RateLimiterId: Parameter + Member + Copy + MaybeSerializeDeserialize + Ord + TypeInfo;

		/// The maximum number of KeyFilter configured to a RateLimiterId.
		#[pallet::constant]
		type MaxWhitelistFilterCount: Get<u32>;

		/// Time used for calculate quota.
		type UnixTime: UnixTime;

		// /// Weight information for the extrinsics in this module.
		// type WeightInfo: WeightInfo;
	}

	#[pallet::error]
	pub enum Error<T> {
		/// Invalid rate limit rule.
		InvalidRateLimitRule,
		/// The KeyFilter has been existed already.
		FilterExisted,
		/// The KeyFilter doesn't exist.
		FilterNotExisted,
		/// Exceed the allowed maximum number of KeyFilter configured to a
		/// RateLimiterId.
		MaxFilterExceeded,
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub(crate) fn deposit_event)]
	pub enum Event<T: Config> {
		/// The rate limit rule has updated.
		RateLimitRuleUpdated {
			rate_limiter_id: T::RateLimiterId,
			encoded_key: Vec<u8>,
			update: Option<RateLimitRule>,
		},
		/// The whitelist of bypass rate limit has been added new KeyFilter.
		WhitelistFilterAdded { rate_limiter_id: T::RateLimiterId },
		/// The whitelist of bypass rate limit has been removed a KeyFilter.
		WhitelistFilterRemoved { rate_limiter_id: T::RateLimiterId },
		/// The whitelist of bypass rate limit has been reset.
		WhitelistFilterReset { rate_limiter_id: T::RateLimiterId },
	}

	/// The rate limit rule for specific RateLimiterId and encoded key.
	///
	/// RateLimitRules: double_map RateLimiterId, EncodedKey => RateLimitRule
	#[pallet::storage]
	#[pallet::getter(fn rate_limit_rules)]
	pub type RateLimitRules<T: Config> =
		StorageDoubleMap<_, Twox64Concat, T::RateLimiterId, Twox64Concat, Vec<u8>, RateLimitRule, OptionQuery>;

	/// The quota for specific RateLimiterId and encoded key.
	///
	/// RateLimitQuota: double_map RateLimiterId, EncodedKey =>
	/// (LastUpdatedBlockOrTime, RemainerQuota)
	#[pallet::storage]
	#[pallet::getter(fn rate_limit_quota)]
	pub type RateLimitQuota<T: Config> =
		StorageDoubleMap<_, Twox64Concat, T::RateLimiterId, Twox64Concat, Vec<u8>, (u64, u128), ValueQuery>;

	/// The rules to filter if key is in whitelist for specific RateLimiterId.
	///
	/// BypassLimitWhitelist: map RateLimiterId => Vec<KeyFilter>
	#[pallet::storage]
	#[pallet::getter(fn bypass_limit_whitelist)]
	pub type BypassLimitWhitelist<T: Config> =
		StorageMap<_, Twox64Concat, T::RateLimiterId, BoundedVec<KeyFilter, T::MaxWhitelistFilterCount>, ValueQuery>;

	#[pallet::pallet]
	#[pallet::without_storage_info]
	pub struct Pallet<T>(_);

	#[pallet::hooks]
	impl<T: Config> Hooks<T::BlockNumber> for Pallet<T> {}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Config the rate limit rule.
		///
		/// Requires `GovernanceOrigin`
		///
		/// Parameters:
		/// - `rate_limiter_id`: rate limiter id.
		/// - `encoded key`: the encoded key to limit.
		/// - `update`: the RateLimitRule to config, None will remove current
		///   config.
		#[pallet::weight(10000)]
		#[transactional]
		pub fn update_rate_limit_rule(
			origin: OriginFor<T>,
			rate_limiter_id: T::RateLimiterId,
			encoded_key: Vec<u8>,
			update: Option<RateLimitRule>,
		) -> DispatchResult {
			T::GovernanceOrigin::ensure_origin(origin)?;

			RateLimitRules::<T>::try_mutate_exists(
				&rate_limiter_id,
				encoded_key.clone(),
				|maybe_limit| -> DispatchResult {
					*maybe_limit = update.clone();

					if let Some(rule) = maybe_limit {
						match rule {
							RateLimitRule::PerBlocks { blocks_count, quota } => {
								ensure!(
									!blocks_count.is_zero() && !quota.is_zero(),
									Error::<T>::InvalidRateLimitRule
								);
							}
							RateLimitRule::PerSeconds { secs_count, quota } => {
								ensure!(
									!secs_count.is_zero() && !quota.is_zero(),
									Error::<T>::InvalidRateLimitRule
								);
							}
							RateLimitRule::TokenBucket {
								blocks_count,
								quota_increment,
								max_quota,
							} => {
								ensure!(
									!blocks_count.is_zero() && !quota_increment.is_zero() && !max_quota.is_zero(),
									Error::<T>::InvalidRateLimitRule
								);
							}
							_ => {}
						}
					}

					// always reset RateLimitQuota.
					RateLimitQuota::<T>::remove(&rate_limiter_id, &encoded_key);

					Self::deposit_event(Event::RateLimitRuleUpdated {
						rate_limiter_id,
						encoded_key,
						update,
					});

					Ok(())
				},
			)
		}

		/// Add whitelist filter rule.
		///
		/// Requires `GovernanceOrigin`
		///
		/// Parameters:
		/// - `rate_limiter_id`: rate limiter id.
		/// - `key_filter`: filter rule to add.
		#[pallet::weight(10000)]
		#[transactional]
		pub fn add_whitelist(
			origin: OriginFor<T>,
			rate_limiter_id: T::RateLimiterId,
			key_filter: KeyFilter,
		) -> DispatchResult {
			T::GovernanceOrigin::ensure_origin(origin)?;

			BypassLimitWhitelist::<T>::try_mutate(rate_limiter_id, |whitelist| -> DispatchResult {
				let location = whitelist
					.binary_search(&key_filter)
					.err()
					.ok_or(Error::<T>::FilterExisted)?;
				whitelist
					.try_insert(location, key_filter)
					.map_err(|_| Error::<T>::MaxFilterExceeded)?;

				Self::deposit_event(Event::WhitelistFilterAdded { rate_limiter_id });
				Ok(())
			})
		}

		/// Remove whitelist filter rule.
		///
		/// Requires `GovernanceOrigin`
		///
		/// Parameters:
		/// - `rate_limiter_id`: rate limiter id.
		/// - `key_filter`: filter rule to remove.
		#[pallet::weight(10000)]
		#[transactional]
		pub fn remove_whitelist(
			origin: OriginFor<T>,
			rate_limiter_id: T::RateLimiterId,
			key_filter: KeyFilter,
		) -> DispatchResult {
			T::GovernanceOrigin::ensure_origin(origin)?;

			BypassLimitWhitelist::<T>::try_mutate(rate_limiter_id, |whitelist| -> DispatchResult {
				let location = whitelist
					.binary_search(&key_filter)
					.ok()
					.ok_or(Error::<T>::FilterExisted)?;
				whitelist.remove(location);

				Self::deposit_event(Event::WhitelistFilterRemoved { rate_limiter_id });
				Ok(())
			})
		}

		/// Resett whitelist filter rule.
		///
		/// Requires `GovernanceOrigin`
		///
		/// Parameters:
		/// - `rate_limiter_id`: rate limiter id.
		/// - `new_list`: the filter rule list to reset.
		#[pallet::weight(10000)]
		#[transactional]
		pub fn reset_whitelist(
			origin: OriginFor<T>,
			rate_limiter_id: T::RateLimiterId,
			new_list: Vec<KeyFilter>,
		) -> DispatchResult {
			T::GovernanceOrigin::ensure_origin(origin)?;

			let mut whitelist: BoundedVec<KeyFilter, T::MaxWhitelistFilterCount> =
				BoundedVec::try_from(new_list).map_err(|_| Error::<T>::MaxFilterExceeded)?;
			whitelist.sort();
			BypassLimitWhitelist::<T>::insert(rate_limiter_id, whitelist);

			Self::deposit_event(Event::WhitelistFilterReset { rate_limiter_id });
			Ok(())
		}
	}

	impl<T: Config> Pallet<T> {
		/// Access the RateLimitQuota, if RateLimitRule will produce new quota,
		/// update RateLimitQuota and then return remainer_quota
		pub fn access_remainer_quota_after_update(
			rate_limit_rule: RateLimitRule,
			limiter_id: &T::RateLimiterId,
			encoded_key: &Vec<u8>,
		) -> u128 {
			RateLimitQuota::<T>::mutate(limiter_id, encoded_key, |(last_updated, remainer_quota)| -> u128 {
				match rate_limit_rule {
					RateLimitRule::PerBlocks { blocks_count, quota } => {
						let now: u64 = frame_system::Pallet::<T>::block_number().saturated_into();
						let interval: u64 = now.saturating_sub(*last_updated);
						if interval >= blocks_count {
							*last_updated = now;
							*remainer_quota = quota;
						}
					}

					RateLimitRule::PerSeconds { secs_count, quota } => {
						let now: u64 = T::UnixTime::now().as_secs();
						let interval: u64 = now.saturating_sub(*last_updated);
						if interval >= secs_count {
							*last_updated = now;
							*remainer_quota = quota;
						}
					}

					RateLimitRule::TokenBucket {
						blocks_count,
						quota_increment,
						max_quota,
					} => {
						let now: u64 = frame_system::Pallet::<T>::block_number().saturated_into();
						let interval: u64 = now.saturating_sub(*last_updated);
						if !blocks_count.is_zero() && interval >= blocks_count {
							let inc_times: u128 = interval
								.checked_div(blocks_count)
								.expect("already ensure blocks_count is not zero; qed")
								.saturated_into();

							*last_updated = now;
							*remainer_quota = quota_increment
								.saturating_mul(inc_times)
								.saturating_add(*remainer_quota)
								.min(max_quota);
						}
					}

					_ => {}
				}

				*remainer_quota
			})
		}
	}

	impl<T: Config> RateLimiter for Pallet<T> {
		type RateLimiterId = T::RateLimiterId;

		fn bypass_limit(limiter_id: Self::RateLimiterId, key: impl Encode) -> bool {
			let encode_key: Vec<u8> = key.encode();

			for key_filter in BypassLimitWhitelist::<T>::get(limiter_id) {
				match key_filter {
					KeyFilter::Match(vec) => {
						if encode_key == vec {
							return true;
						}
					}
					KeyFilter::StartsWith(prefix) => {
						if encode_key.starts_with(&prefix) {
							return true;
						}
					}
					KeyFilter::EndsWith(postfix) => {
						if encode_key.ends_with(&postfix) {
							return true;
						}
					}
				}
			}

			false
		}

		fn is_allowed(limiter_id: Self::RateLimiterId, key: impl Encode, value: u128) -> Result<(), RateLimiterError> {
			let encoded_key: Vec<u8> = key.encode();

			let allowed = match RateLimitRules::<T>::get(&limiter_id, &encoded_key) {
				Some(rate_limit_rule @ RateLimitRule::PerBlocks { .. })
				| Some(rate_limit_rule @ RateLimitRule::PerSeconds { .. })
				| Some(rate_limit_rule @ RateLimitRule::TokenBucket { .. }) => {
					let remainer_quota =
						Self::access_remainer_quota_after_update(rate_limit_rule, &limiter_id, &encoded_key);

					value <= remainer_quota
				}
				Some(RateLimitRule::Unlimited) => true,
				Some(RateLimitRule::NotAllowed) => {
					// always return false, even if the value is zero.
					false
				}
				None => {
					// if doesn't rate limit rule, always return true.
					true
				}
			};

			ensure!(allowed, RateLimiterError::ExceedLimit);

			Ok(())
		}

		fn record(limiter_id: Self::RateLimiterId, key: impl Encode, value: u128) {
			let encoded_key: Vec<u8> = key.encode();

			match RateLimitRules::<T>::get(&limiter_id, &encoded_key) {
				Some(RateLimitRule::PerBlocks { .. })
				| Some(RateLimitRule::PerSeconds { .. })
				| Some(RateLimitRule::TokenBucket { .. }) => {
					// consume remainer quota in these situation.
					RateLimitQuota::<T>::mutate(&limiter_id, &encoded_key, |(_, remainer_quota)| {
						*remainer_quota = (*remainer_quota).saturating_sub(value);
					});
				}
				_ => {}
			};
		}
	}
}