use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const WEIGHT_EPSILON: f32 = 0.001;
const QUAT_EPSILON: f32 = 0.002;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssetDocument {
    pub id: Uuid,
    pub name: String,
    pub version: u64,
    pub meshes: Vec<Mesh>,
    pub skeleton: Skeleton,
    pub animations: Vec<AnimationClip>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mesh {
    pub id: Uuid,
    pub name: String,
    pub vertices: Vec<Vertex>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Vertex {
    pub position: [f32; 3],
    pub joint_indices: [u16; 4],
    pub weights: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skeleton {
    pub joints: Vec<Joint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Joint {
    pub name: String,
    pub parent_index: Option<usize>,
    pub rest_translation: [f32; 3],
    /// Quaternion in XYZW order.
    pub rest_rotation: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnimationClip {
    pub id: Uuid,
    pub name: String,
    pub duration_ms: u64,
    pub tracks: Vec<AnimationTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackChannel {
    Translation,
    Rotation,
    Scale,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnimationTrack {
    pub joint_index: usize,
    pub channel: TrackChannel,
    pub keyframes: Vec<Keyframe>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Keyframe {
    pub time_ms: u64,
    /// Translation/scale use XYZ; rotation uses quaternion XYZW.
    pub value: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Influence {
    pub joint_index: u16,
    pub weight: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditOperation {
    SetVertexPosition {
        mesh_id: Uuid,
        vertex_index: usize,
        position: [f32; 3],
    },
    SetSkinWeights {
        mesh_id: Uuid,
        vertex_index: usize,
        influences: Vec<Influence>,
    },
    UpsertKeyframe {
        clip_id: Uuid,
        joint_index: usize,
        channel: TrackChannel,
        time_ms: u64,
        value: [f32; 4],
    },
    ShiftKeyframes {
        clip_id: Uuid,
        joint_index: usize,
        channel: TrackChannel,
        from_ms: u64,
        delta_ms: i64,
    },
}

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("invalid asset: {0}")]
    InvalidAsset(String),
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("invalid operation: {0}")]
    InvalidOperation(String),
}

impl AssetDocument {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.name.trim().is_empty() {
            return Err(DomainError::InvalidAsset("asset name is empty".into()));
        }
        if self.skeleton.joints.is_empty() {
            return Err(DomainError::InvalidAsset("skeleton must contain at least one joint".into()));
        }

        for (index, joint) in self.skeleton.joints.iter().enumerate() {
            if let Some(parent) = joint.parent_index {
                if parent >= self.skeleton.joints.len() || parent == index {
                    return Err(DomainError::InvalidAsset(format!(
                        "joint {index} has invalid parent index {parent}"
                    )));
                }
            }
            if !finite3(joint.rest_translation) || !finite4(joint.rest_rotation) {
                return Err(DomainError::InvalidAsset(format!("joint {index} contains non-finite values")));
            }
            if !is_unit_quaternion(joint.rest_rotation, QUAT_EPSILON) {
                return Err(DomainError::InvalidAsset(format!("joint {index} rest rotation is not a unit quaternion")));
            }
        }

        for mesh in &self.meshes {
            if mesh.name.trim().is_empty() {
                return Err(DomainError::InvalidAsset("mesh name is empty".into()));
            }
            for (vertex_index, vertex) in mesh.vertices.iter().enumerate() {
                if !finite3(vertex.position) || !vertex.weights.iter().all(|v| v.is_finite()) {
                    return Err(DomainError::InvalidAsset(format!(
                        "mesh {} vertex {vertex_index} contains non-finite values",
                        mesh.name
                    )));
                }
                if vertex.weights.iter().any(|weight| *weight < 0.0) {
                    return Err(DomainError::InvalidAsset(format!(
                        "mesh {} vertex {vertex_index} contains a negative weight",
                        mesh.name
                    )));
                }
                for slot in 0..4 {
                    if vertex.weights[slot] > 0.0
                        && usize::from(vertex.joint_indices[slot]) >= self.skeleton.joints.len()
                    {
                        return Err(DomainError::InvalidAsset(format!(
                            "mesh {} vertex {vertex_index} references missing joint {}",
                            mesh.name, vertex.joint_indices[slot]
                        )));
                    }
                }
                let sum: f32 = vertex.weights.iter().sum();
                if sum > 0.0 && (sum - 1.0).abs() > WEIGHT_EPSILON {
                    return Err(DomainError::InvalidAsset(format!(
                        "mesh {} vertex {vertex_index} weights sum to {sum}, expected 1.0",
                        mesh.name
                    )));
                }
            }
        }

        for clip in &self.animations {
            for track in &clip.tracks {
                if track.joint_index >= self.skeleton.joints.len() {
                    return Err(DomainError::InvalidAsset(format!(
                        "clip {} references missing joint {}",
                        clip.name, track.joint_index
                    )));
                }
                let mut previous: Option<u64> = None;
                for keyframe in &track.keyframes {
                    if keyframe.time_ms > clip.duration_ms {
                        return Err(DomainError::InvalidAsset(format!(
                            "clip {} keyframe exceeds clip duration",
                            clip.name
                        )));
                    }
                    if previous.is_some_and(|time| keyframe.time_ms <= time) {
                        return Err(DomainError::InvalidAsset(format!(
                            "clip {} keyframes are not strictly increasing",
                            clip.name
                        )));
                    }
                    previous = Some(keyframe.time_ms);
                    if !finite4(keyframe.value) {
                        return Err(DomainError::InvalidAsset("keyframe contains non-finite values".into()));
                    }
                    if track.channel == TrackChannel::Rotation
                        && !is_unit_quaternion(keyframe.value, QUAT_EPSILON)
                    {
                        return Err(DomainError::InvalidAsset("rotation keyframe is not a unit quaternion".into()));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn apply_operations(&self, operations: &[EditOperation]) -> Result<Self, DomainError> {
        let mut candidate = self.clone();
        for operation in operations {
            candidate.apply_operation(operation)?;
        }
        candidate.validate()?;
        Ok(candidate)
    }

    fn apply_operation(&mut self, operation: &EditOperation) -> Result<(), DomainError> {
        match operation {
            EditOperation::SetVertexPosition { mesh_id, vertex_index, position } => {
                if !finite3(*position) {
                    return Err(DomainError::InvalidOperation("position must be finite".into()));
                }
                let mesh = self.meshes.iter_mut().find(|mesh| mesh.id == *mesh_id)
                    .ok_or_else(|| DomainError::TargetNotFound(format!("mesh {mesh_id}")))?;
                let vertex = mesh.vertices.get_mut(*vertex_index)
                    .ok_or_else(|| DomainError::TargetNotFound(format!("vertex {vertex_index}")))?;
                vertex.position = *position;
            }
            EditOperation::SetSkinWeights { mesh_id, vertex_index, influences } => {
                if influences.is_empty() || influences.len() > 4 {
                    return Err(DomainError::InvalidOperation(
                        "MVP supports one to four joint influences per vertex".into()
                    ));
                }
                let mut sum = 0.0_f32;
                for influence in influences {
                    if usize::from(influence.joint_index) >= self.skeleton.joints.len() {
                        return Err(DomainError::InvalidOperation(format!(
                            "joint index {} is outside the skeleton",
                            influence.joint_index
                        )));
                    }
                    if !influence.weight.is_finite() || influence.weight < 0.0 {
                        return Err(DomainError::InvalidOperation("weights must be finite and non-negative".into()));
                    }
                    sum += influence.weight;
                }
                if sum <= f32::EPSILON {
                    return Err(DomainError::InvalidOperation("influence weights must have a positive sum".into()));
                }

                let mesh = self.meshes.iter_mut().find(|mesh| mesh.id == *mesh_id)
                    .ok_or_else(|| DomainError::TargetNotFound(format!("mesh {mesh_id}")))?;
                let vertex = mesh.vertices.get_mut(*vertex_index)
                    .ok_or_else(|| DomainError::TargetNotFound(format!("vertex {vertex_index}")))?;
                vertex.joint_indices = [0; 4];
                vertex.weights = [0.0; 4];
                for (slot, influence) in influences.iter().enumerate() {
                    vertex.joint_indices[slot] = influence.joint_index;
                    vertex.weights[slot] = influence.weight / sum;
                }
            }
            EditOperation::UpsertKeyframe { clip_id, joint_index, channel, time_ms, value } => {
                if *joint_index >= self.skeleton.joints.len() {
                    return Err(DomainError::InvalidOperation(format!("joint index {joint_index} does not exist")));
                }
                let clip = self.animations.iter_mut().find(|clip| clip.id == *clip_id)
                    .ok_or_else(|| DomainError::TargetNotFound(format!("clip {clip_id}")))?;
                if *time_ms > clip.duration_ms {
                    return Err(DomainError::InvalidOperation("keyframe time exceeds clip duration".into()));
                }
                let mut normalized = *value;
                if !finite4(normalized) {
                    return Err(DomainError::InvalidOperation("keyframe value must be finite".into()));
                }
                if *channel == TrackChannel::Rotation {
                    normalized = normalize_quaternion(normalized)?;
                }
                let track = if let Some(track) = clip.tracks.iter_mut()
                    .find(|track| track.joint_index == *joint_index && track.channel == *channel)
                {
                    track
                } else {
                    clip.tracks.push(AnimationTrack {
                        joint_index: *joint_index,
                        channel: channel.clone(),
                        keyframes: Vec::new(),
                    });
                    clip.tracks.last_mut().expect("track was just inserted")
                };
                if let Some(existing) = track.keyframes.iter_mut().find(|frame| frame.time_ms == *time_ms) {
                    existing.value = normalized;
                } else {
                    track.keyframes.push(Keyframe { time_ms: *time_ms, value: normalized });
                    track.keyframes.sort_by_key(|frame| frame.time_ms);
                }
            }
            EditOperation::ShiftKeyframes { clip_id, joint_index, channel, from_ms, delta_ms } => {
                let clip = self.animations.iter_mut().find(|clip| clip.id == *clip_id)
                    .ok_or_else(|| DomainError::TargetNotFound(format!("clip {clip_id}")))?;
                let track = clip.tracks.iter_mut()
                    .find(|track| track.joint_index == *joint_index && track.channel == *channel)
                    .ok_or_else(|| DomainError::TargetNotFound("animation track".into()))?;
                for frame in track.keyframes.iter_mut().filter(|frame| frame.time_ms >= *from_ms) {
                    let shifted = i128::from(frame.time_ms) + i128::from(*delta_ms);
                    if shifted < 0 || shifted > i128::from(clip.duration_ms) {
                        return Err(DomainError::InvalidOperation("shifted keyframe falls outside clip duration".into()));
                    }
                    frame.time_ms = shifted as u64;
                }
                track.keyframes.sort_by_key(|frame| frame.time_ms);
            }
        }
        Ok(())
    }
}

fn finite3(value: [f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}
fn finite4(value: [f32; 4]) -> bool {
    value.iter().all(|component| component.is_finite())
}
fn is_unit_quaternion(value: [f32; 4], epsilon: f32) -> bool {
    let norm = value.iter().map(|component| component * component).sum::<f32>().sqrt();
    (norm - 1.0).abs() <= epsilon
}
fn normalize_quaternion(value: [f32; 4]) -> Result<[f32; 4], DomainError> {
    let norm = value.iter().map(|component| component * component).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(DomainError::InvalidOperation("rotation quaternion must have non-zero length".into()));
    }
    Ok([value[0] / norm, value[1] / norm, value[2] / norm, value[3] / norm])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_asset() -> AssetDocument {
        AssetDocument {
            id: Uuid::from_u128(1),
            name: "sample".into(),
            version: 1,
            meshes: vec![Mesh {
                id: Uuid::from_u128(2),
                name: "body".into(),
                vertices: vec![Vertex {
                    position: [0.0, 0.0, 0.0],
                    joint_indices: [0; 4],
                    weights: [1.0, 0.0, 0.0, 0.0],
                }],
            }],
            skeleton: Skeleton {
                joints: vec![Joint {
                    name: "root".into(),
                    parent_index: None,
                    rest_translation: [0.0, 0.0, 0.0],
                    rest_rotation: [0.0, 0.0, 0.0, 1.0],
                }],
            },
            animations: vec![AnimationClip {
                id: Uuid::from_u128(3),
                name: "idle".into(),
                duration_ms: 1000,
                tracks: vec![AnimationTrack {
                    joint_index: 0,
                    channel: TrackChannel::Rotation,
                    keyframes: vec![Keyframe { time_ms: 0, value: [0.0, 0.0, 0.0, 1.0] }],
                }],
            }],
        }
    }

    #[test]
    fn validates_sample_asset() {
        assert!(sample_asset().validate().is_ok());
    }

    #[test]
    fn skin_weights_are_normalized() {
        let operation = EditOperation::SetSkinWeights {
            mesh_id: Uuid::from_u128(2),
            vertex_index: 0,
            influences: vec![Influence { joint_index: 0, weight: 2.0 }],
        };
        let updated = sample_asset().apply_operations(&[operation]).unwrap();
        assert!((updated.meshes[0].vertices[0].weights[0] - 1.0).abs() < 0.0001);
    }

    #[test]
    fn rejects_missing_joint_influence() {
        let operation = EditOperation::SetSkinWeights {
            mesh_id: Uuid::from_u128(2),
            vertex_index: 0,
            influences: vec![Influence { joint_index: 9, weight: 1.0 }],
        };
        assert!(sample_asset().apply_operations(&[operation]).is_err());
    }

    #[test]
    fn keyframe_upsert_sorts_and_normalizes_quaternion() {
        let operation = EditOperation::UpsertKeyframe {
            clip_id: Uuid::from_u128(3),
            joint_index: 0,
            channel: TrackChannel::Rotation,
            time_ms: 500,
            value: [0.0, 0.0, 0.0, 2.0],
        };
        let updated = sample_asset().apply_operations(&[operation]).unwrap();
        let frames = &updated.animations[0].tracks[0].keyframes;
        assert_eq!(frames[1].time_ms, 500);
        assert!((frames[1].value[3] - 1.0).abs() < 0.0001);
    }

    #[test]
    fn rejects_shift_outside_clip() {
        let operation = EditOperation::ShiftKeyframes {
            clip_id: Uuid::from_u128(3),
            joint_index: 0,
            channel: TrackChannel::Rotation,
            from_ms: 0,
            delta_ms: -1,
        };
        assert!(sample_asset().apply_operations(&[operation]).is_err());
    }
}
