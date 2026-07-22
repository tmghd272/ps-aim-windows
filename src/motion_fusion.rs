//! Gyro-to-mouse sensor fusion, based on the algorithm in JibbSmart's
//! GamepadMotionHelpers (https://github.com/JibbSmart/GamepadMotionHelpers,
//! MIT licensed) -- the same library BetterJoy uses for Joy-Con gyro
//! mouse support.
//!
//! WHAT'S FAITHFULLY PORTED vs RECONSTRUCTED:
//! - `MotionState::update()` is a faithful port of the real
//!   `Motion::Update()` source we fetched directly from the project's
//!   GitHub repo: it integrates gyro rotation into a quaternion, and
//!   separately tracks a gravity vector that's corrected toward the
//!   accelerometer reading at a rate based on how "shaky" recent motion
//!   has been (so a fast swing doesn't yank the gravity estimate off
//!   course, but a held-still moment corrects it quickly).
//! - `player_space_gyro()` is NOT a direct port -- GitHub's raw-file
//!   endpoint blocked automated fetching and the rendered page
//!   truncated partway through the file, so we only had a fragment of
//!   the original CalculatePlayerSpaceGyro formula, not its complete
//!   body. This is my own reconstruction using the same underlying
//!   principle their fragment demonstrated: project the gyro vector
//!   onto the tracked gravity direction to get a "rotation around
//!   gravity" (yaw) component that stays meaningful regardless of how
//!   the controller is physically tilted, then take the perpendicular
//!   remainder as pitch. Worth treating this specific function as a
//!   first attempt at the real technique, not a guaranteed-exact match
//!   to the original library's output.
//!
//! Units: gyro in degrees/second, accel in g-force (1g = usual gravity).
//! Real hardware reports raw firmware units for both -- conversion
//! factors below are estimated from observed data (accel baseline ~4100
//! raw units when stationary ~= 1g) and a commonly-cited PS4-style MEMS
//! gyro sensitivity, not from an official datasheet. May need tuning.

pub const RAW_ACCEL_TO_G: f32 = 1.0 / 4100.0;
pub const RAW_GYRO_TO_DEG_PER_SEC: f32 = 1.0 / 16.0;

#[derive(Clone, Copy, Debug)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }
    pub fn zero() -> Self {
        Vec3::new(0.0, 0.0, 0.0)
    }
    pub fn length(&self) -> f32 {
        self.length_squared().sqrt()
    }
    pub fn length_squared(&self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }
    pub fn normalized(&self) -> Vec3 {
        let len = self.length();
        if len == 0.0 {
            *self
        } else {
            Vec3::new(self.x / len, self.y / len, self.z / len)
        }
    }
    pub fn dot(&self, other: Vec3) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }
    pub fn cross(&self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }
    pub fn scale(&self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
    pub fn add(&self, other: Vec3) -> Vec3 {
        Vec3::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
    pub fn sub(&self, other: Vec3) -> Vec3 {
        Vec3::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
    pub fn lerp(&self, other: Vec3, factor: f32) -> Vec3 {
        self.add(other.sub(*self).scale(factor))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Quat {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Quat {
    pub fn identity() -> Self {
        Quat { w: 1.0, x: 0.0, y: 0.0, z: 0.0 }
    }

    pub fn angle_axis(angle: f32, axis: Vec3) -> Self {
        let axis = axis.normalized();
        let half = angle * 0.5;
        let s = half.sin();
        Quat { w: half.cos(), x: axis.x * s, y: axis.y * s, z: axis.z * s }
    }

    pub fn multiply(&self, rhs: Quat) -> Quat {
        Quat {
            w: self.w * rhs.w - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
            x: self.w * rhs.x + self.x * rhs.w + self.y * rhs.z - self.z * rhs.y,
            y: self.w * rhs.y - self.x * rhs.z + self.y * rhs.w + self.z * rhs.x,
            z: self.w * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.w,
        }
    }

    pub fn normalize(&mut self) {
        let len = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        if len > 0.0 {
            let f = 1.0 / len;
            self.w *= f;
            self.x *= f;
            self.y *= f;
            self.z *= f;
        }
    }

    pub fn inverse(&self) -> Quat {
        // Assumes a unit quaternion (true after normalize()) -- inverse
        // is just the conjugate.
        Quat { w: self.w, x: -self.x, y: -self.y, z: -self.z }
    }

    /// Rotates a vector by this quaternion.
    pub fn rotate_vec(&self, v: Vec3) -> Vec3 {
        let qv = Quat { w: 0.0, x: v.x, y: v.y, z: v.z };
        let result = self.multiply(qv).multiply(self.inverse());
        Vec3::new(result.x, result.y, result.z)
    }
}

pub struct MotionState {
    pub orientation: Quat,
    /// Current estimated gravity direction, in the controller's own
    /// (local/sensor) coordinate space -- NOT world space. This is
    /// what lets us know "which way is down" regardless of how the
    /// controller is currently being held.
    pub grav: Vec3,
    /// Slowly-adapting estimate of the gyro's own resting bias --
    /// real sensors report a small non-zero value even sitting
    /// perfectly still, and without subtracting it out, that constant
    /// offset gets integrated into orientation over time, causing slow
    /// drift. Adapts only when the raw reading looks like it's likely
    /// stationary, so it doesn't also calibrate away genuine slow
    /// movements.
    gyro_bias: Vec3,
    smooth_accel: Vec3,
    shakiness: f32,
}

impl MotionState {
    pub fn new() -> Self {
        MotionState {
            orientation: Quat::identity(),
            grav: Vec3::zero(),
            gyro_bias: Vec3::zero(),
            smooth_accel: Vec3::zero(),
            shakiness: 0.0,
        }
    }

    /// Call once per frame with the raw gyro reading (already converted
    /// to degrees/second) before using it anywhere else. Adapts the
    /// bias estimate when the reading looks like the controller is
    /// likely stationary, and returns the bias-corrected value to use
    /// for both update() and player_space_gyro().
    pub fn correct_gyro(&mut self, raw_gyro: Vec3, dt: f32) -> Vec3 {
        // Threshold in degrees/second -- below this, assume any
        // remaining reading is sensor bias/noise rather than genuine
        // slow movement. Adaptation rate is deliberately slow so a
        // brief pause mid-motion doesn't get miscalibrated as "this is
        // the new bias."
        const STILLNESS_THRESHOLD: f32 = 2.0;
        const BIAS_ADAPT_RATE: f32 = 0.02;

        if raw_gyro.length() < STILLNESS_THRESHOLD {
            let factor = (BIAS_ADAPT_RATE * dt).clamp(0.0, 1.0);
            self.gyro_bias = self.gyro_bias.lerp(raw_gyro, factor);
        }

        raw_gyro.sub(self.gyro_bias)
    }

    /// Faithful port of Motion::Update from GamepadMotionHelpers.
    /// gyro in degrees/second (already bias-corrected via
    /// correct_gyro()), accel in g-force, dt in seconds.
    pub fn update(&mut self, gyro: Vec3, accel: Vec3, gravity_length: f32, dt: f32) {
        // Tunables from the original GamepadMotionSettings defaults.
        const SHAKY_MIN: f32 = 0.01;
        const SHAKY_MAX: f32 = 0.4;
        const STILL_SPEED: f32 = 1.0;
        const SHAKY_SPEED: f32 = 0.1;
        const GYRO_FACTOR: f32 = 0.1;
        const GYRO_MIN_THRESHOLD: f32 = 0.05;
        const GYRO_MAX_THRESHOLD: f32 = 0.25;
        const MIN_CORRECTION_SPEED: f32 = 0.01;
        const SHORT_STEADINESS_HALF_TIME: f32 = 0.25;

        let angle_speed = gyro.length() * std::f32::consts::PI / 180.0;
        let angle = angle_speed * dt;
        let rotation = Quat::angle_axis(angle, gyro);
        self.orientation = self.orientation.multiply(rotation);

        let accel_magnitude = accel.length();
        if accel_magnitude > 0.0 {
            let accel_norm = accel.scale(1.0 / accel_magnitude);
            let rot_inv = rotation.inverse();

            self.smooth_accel = rot_inv.rotate_vec(self.smooth_accel);
            let smooth_factor = if SHORT_STEADINESS_HALF_TIME <= 0.0 {
                0.0
            } else {
                2f32.powf(-dt / SHORT_STEADINESS_HALF_TIME)
            };
            self.shakiness *= smooth_factor;
            self.shakiness = self.shakiness.max(accel.sub(self.smooth_accel).length());
            self.smooth_accel = accel.lerp(self.smooth_accel, smooth_factor);

            self.grav = rot_inv.rotate_vec(self.grav);

            let grav_to_accel = accel_norm.scale(-gravity_length).sub(self.grav);
            let grav_to_accel_dir = grav_to_accel.normalized();

            let grav_correction_speed = if SHAKY_MIN < SHAKY_MAX {
                STILL_SPEED
                    + (SHAKY_SPEED - STILL_SPEED)
                        * ((self.shakiness - SHAKY_MIN) / (SHAKY_MAX - SHAKY_MIN)).clamp(0.0, 1.0)
            } else if self.shakiness < SHAKY_MAX {
                STILL_SPEED
            } else {
                SHAKY_SPEED
            };

            let gyro_limit = (angle_speed * GYRO_FACTOR).max(MIN_CORRECTION_SPEED);
            let grav_correction_speed = if grav_correction_speed > gyro_limit {
                let close_enough = if GYRO_MIN_THRESHOLD < GYRO_MAX_THRESHOLD {
                    ((grav_to_accel.length() - GYRO_MIN_THRESHOLD)
                        / (GYRO_MAX_THRESHOLD - GYRO_MIN_THRESHOLD))
                        .clamp(0.0, 1.0)
                } else if grav_to_accel.length() < GYRO_MAX_THRESHOLD {
                    0.0
                } else {
                    1.0
                };
                gyro_limit + (grav_correction_speed - gyro_limit) * close_enough
            } else {
                grav_correction_speed
            };

            let grav_to_accel_delta = grav_to_accel_dir.scale(grav_correction_speed * dt);
            if grav_to_accel_delta.length_squared() < grav_to_accel.length_squared() {
                self.grav = self.grav.add(grav_to_accel_delta);
            } else {
                self.grav = accel_norm.scale(-gravity_length);
            }

            // Correct orientation so tracked gravity points straight
            // down in world space -- this is what prevents long-term
            // drift in the "which way is down" estimate.
            let gravity_direction = self.orientation.inverse().rotate_vec(self.grav.normalized());
            let down = Vec3::new(0.0, -1.0, 0.0);
            let error_angle = down.dot(gravity_direction).clamp(-1.0, 1.0).acos();
            let flattened = down.cross(gravity_direction);
            let correction = Quat::angle_axis(error_angle, flattened);
            self.orientation = self.orientation.multiply(correction);
        } else {
            self.grav = rotation.inverse().rotate_vec(self.grav);
        }

        self.orientation.normalize();
    }

    /// NOT a faithful port -- see module doc comment. Splits the raw
    /// gyro reading into a component around the tracked gravity axis
    /// (yaw, robust to controller tilt) and the perpendicular remainder
    /// (pitch). Returns (yaw_rate, pitch_rate) in the same units as the
    /// input gyro (degrees/second if converted properly beforehand).
    pub fn player_space_gyro(&self, gyro: Vec3) -> (f32, f32) {
        let grav_norm = self.grav.normalized();
        if grav_norm.length_squared() < 0.5 {
            // Gravity not yet established (e.g. very first frames) --
            // fall back to raw gyro on two arbitrary perpendicular axes
            // rather than dividing by a near-zero vector.
            return (gyro.y, gyro.x);
        }

        let yaw = -grav_norm.dot(gyro);

        // Pitch axis derived purely from gravity, NOT from the full
        // orientation quaternion (self.orientation). Using orientation
        // caused a feedback loop: it's continuously updated by the same
        // yaw rotation we're trying to measure, so the "pitch axis"
        // reference itself slowly rotated during a yaw gesture, leaking
        // yaw motion into the pitch output (cross-talk -- a pure
        // left-right motion also producing vertical movement). A fixed
        // local reference, projected perpendicular to gravity, stays
        // stable through a pure yaw since that doesn't change gravity's
        // direction in sensor space at all.
        let reference = Vec3::new(1.0, 0.0, 0.0);
        let mut pitch_axis = reference.sub(grav_norm.scale(reference.dot(grav_norm)));
        if pitch_axis.length_squared() < 0.0001 {
            // Reference happened to be parallel to gravity -- use a
            // different one.
            let alt = Vec3::new(0.0, 0.0, 1.0);
            pitch_axis = alt.sub(grav_norm.scale(alt.dot(grav_norm)));
        }
        let pitch_axis = pitch_axis.normalized();
        let pitch = gyro.dot(pitch_axis);

        (yaw, pitch)
    }
}
