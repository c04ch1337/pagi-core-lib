//! Multimodal / spatial fact primitives.
//!
//! This module is intentionally lightweight: it provides typed structures the rest of the
//! system can exchange without pulling embodiment-specific dependencies into the microkernel.

use nalgebra::Vector3;
use serde::{Deserialize, Serialize};

/// A simple 3D coordinate wrapper.
///
/// Internally uses [`nalgebra::Vector3<f32>`] for downstream math convenience.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vector3D(pub Vector3<f32>);

impl Vector3D {
    /// Convenience constructor.
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self(Vector3::new(x, y, z))
    }

    pub fn x(&self) -> f32 {
        self.0.x
    }

    pub fn y(&self) -> f32 {
        self.0.y
    }

    pub fn z(&self) -> f32 {
        self.0.z
    }
}

/// A multimodal fact representing sensor input.
///
/// `data_hash` is a placeholder reference to large external blobs (image/video/etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MultimodalFact {
    pub sensor_id: String,
    pub timestamp: i64,
    pub location: Vector3D,
    pub data_hash: String,
}

/// A fact representing a physical action executed by the robotics agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoboticsAction {
    pub directive: String,
    pub target_location: Vector3D,
    pub status: String,
}

/// A typed fact payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum FactType {
    MultimodalFact(MultimodalFact),
    RoboticsAction(RoboticsAction),
}

